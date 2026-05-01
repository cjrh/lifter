use std::collections::HashMap;
use std::io::{Read, Write};
#[cfg(target_family = "unix")]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use itertools::Itertools;
use log::*;
use scraper::{Html, Selector};
use strfmt::strfmt;
use url::Url;

mod archive;
mod btlog;
mod gzfile;
pub mod reporter;
mod tarfile;
mod tarxzfile;
mod zipfile;

use crate::btlog::log_error_with_stack_trace;
use crate::reporter::{OutputRecord, Reporter};

/// Shared, per-run state passed into every parallel `run_section` call.
/// `config_write` serializes writes to the INI file (tini has no
/// concurrency story). `reporter` serializes rows on stdout.
pub struct RunContext {
    pub config_write: std::sync::Mutex<()>,
    pub reporter: Reporter,
}

impl RunContext {
    pub fn new() -> Self {
        RunContext {
            config_write: std::sync::Mutex::new(()),
            reporter: Reporter::new(),
        }
    }
}

impl Default for RunContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of processing one section. This carries enough context that
/// the caller can report what was *observed* remotely even when no
/// download happened — which is the whole point of the CSV output.
enum Outcome {
    /// A new artifact was downloaded this run.
    Updated {
        version: String,
        commit: Option<String>,
    },
    /// The remote version was found and is not newer than what's on disk.
    UpToDate { version: String },
    /// The remote scrape produced no match at all.
    NoHit,
    /// A version was found but the download URL's extension isn't one we handle.
    ExtUnsupported { version: String },
}

impl Outcome {
    fn updated(&self) -> bool {
        matches!(self, Outcome::Updated { .. })
    }

    fn current_version(&self) -> Option<&str> {
        match self {
            Outcome::Updated { version, .. }
            | Outcome::UpToDate { version }
            | Outcome::ExtUnsupported { version } => Some(version.as_str()),
            Outcome::NoHit => None,
        }
    }
}

/// Build an HTTP agent that surfaces every response (including 4xx/5xx) as
/// `Ok(Response)` so the retry loops can inspect the body on error statuses
/// (notably 403 responses from Github, where the body contains the rate-limit
/// reason).
fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

/// One file the user wants out of an archive.
///
/// `pattern` matches against the basename of an archive entry.
/// `rename_to` is the destination filename on disk:
///   - `Some(name)` — write the matched entry to `name` (singular mode).
///   - `None` — write it under its original archive basename (plural mode).
/// `pattern_str` is the raw, uncompiled pattern, kept around for
/// "does this file already exist on disk?" checks where we treat the
/// pattern as a literal filename (the realistic case for plural mode).
#[derive(Debug)]
struct ExtractionTarget {
    pattern_str: String,
    pattern: regex::Regex,
    rename_to: Option<String>,
}

impl ExtractionTarget {
    /// Best-effort guess at the on-disk output name without inspecting
    /// the archive. For singular mode that's `rename_to`; for plural
    /// mode we treat the pattern as a literal filename, which is the
    /// expected use of the plural form.
    fn predicted_output_name(&self) -> &str {
        self.rename_to.as_deref().unwrap_or(&self.pattern_str)
    }
}

/// This struct represents a particular artifact that will
/// be downloaded.
#[derive(Default, Debug)]
struct Config {
    method: String,
    template: String,
    version: Option<String>,

    /// More direct strategy
    /// The HTTP page link that contains the download link
    page_url: String,
    /// The download anchor tag selector. The "href" of the tag will be used.
    /// This will likely match many items, e.g. if there are multiple downloads for different
    /// versions and platforms.
    anchor_tag: String,
    /// This will be matched
    anchor_text: String,
    /// The version tag to check. The "text" of the tag will be used.
    version_tag: Option<String>,
    /// The commit tag is used if the "version" always comes
    /// back as the same thing. An example of this is neovim,
    /// where the project keeps using the tag `stable`, but the
    /// commit hash changes. In this case, we'll use the commit
    /// as a disambiguator.
    commit_tag: Option<String>,
    commit: Option<String>,
    /// One or more files to pull out of the downloaded archive.
    /// Singular mode produces exactly one entry (with `rename_to` set);
    /// plural mode produces N entries with `rename_to == None`.
    extraction_targets: Vec<ExtractionTarget>,
}

impl Config {
    fn new() -> Config {
        Config {
            ..Default::default()
        }
    }

    /// The single extraction target if this config was built in
    /// singular mode (one target with an explicit `rename_to`). Used to
    /// gate non-archive downloads (.exe / .gz / .com / .AppImage),
    /// which only make sense in singular mode.
    fn singular_target(&self) -> Option<&ExtractionTarget> {
        match self.extraction_targets.as_slice() {
            [t] if t.rename_to.is_some() => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq)]
struct Hit {
    version: String,
    commit: Option<String>,
    download_url: String,
}

/// Read a section of the config file (ini file) into a hashmap.
/// This isn't too complicated, pretty much uses the iterator
/// provided by tini.
fn read_section_into_map(conf: &tini::Ini, section: &str) -> HashMap<String, String> {
    let mut tmp = HashMap::new();
    conf.section_iter(section).for_each(|(k, v)| {
        tmp.insert(k.clone(), v.clone());
    });
    tmp
}

type Templates = HashMap<String, HashMap<String, String>>;

/// Mutate the config to replace a template with the template values.
///
/// If `template` is specified in a section, we must use it! Look up
/// the fields that are defined in a template with that name, and
/// insert those fields into the `Config` object that represents
/// that section.
///
/// Beyond simple substitution, the individual template values can
/// also themselves be used as templates using the handlebars
/// format. Any fields defined in the *section* can be substituted
/// into each template item if the `{name}` of that item is used.
///
/// Let's look at a real example. Here is a template for a "github"
/// releases page:
///
/// ```ini
/// [template:github_release_latest]
/// page_url = https://github.com/{project}/releases/latest
/// anchor_tag = html main details a
/// version_tag = a.Link--muted span.css-truncate.css-truncate-target span.ml-1
/// ```
///
/// Here is the entry for ripgrep:
///
/// ```ini
/// [ripgrep]
/// template = github_release_latest
/// project = BurntSushi/ripgrep
/// anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
/// target_filename_to_extract_from_archive = rg
/// version = 13.0.0
/// ```
///
/// This function does the following:
///
/// 1. Add the fields in the template (page_url, anchor_tag, version_tag)
///    into the ripgrep config hashmap
/// 2. Sees that the `{project}` template variable appears in the `page_url`
///    field, and substitutes the value of the actual `project` value
///    in the ripgrep section.
fn insert_fields_from_template(
    cf: &mut Config,
    templates: &Templates,
    values: &HashMap<String, String>,
) -> Result<()> {
    if let Some(t) = values.get("template") {
        debug!("Config has a template: {:?}", &values);
        cf.template = t.clone();
        let template_fields = templates.get(&cf.template).ok_or_else(|| {
            anyhow!(
                "The specified template '{}' was not found in the \
                    list of available templates: {:?}",
                &t,
                templates.keys().collect::<Vec<_>>()
            )
        })?;

        if let Some(value) = template_fields.get("page_url") {
            cf.page_url = strfmt(value, values)?;
        };
        if let Some(value) = template_fields.get("anchor_tag") {
            cf.anchor_tag = strfmt(value, values)?;
        };
        if let Some(value) = template_fields.get("version_tag") {
            cf.version_tag = Some(strfmt(value, values)?);
        };
        if let Some(value) = template_fields.get("commit_tag") {
            cf.commit_tag = Some(strfmt(value, values)?);
        };
        if let Some(value) = template_fields.get("method") {
            cf.method = strfmt(value, values)?;
        };
    };

    debug!("Substitutions complete: {:?}", &cf);
    Ok(())
}

/// Process one section. Always emits exactly one CSV row on
/// `ctx.reporter`, even on error (stderr still carries the stack
/// trace). Returns `Ok(())` unconditionally so one failing section
/// doesn't poison the `rayon` iterator.
pub fn run_section(
    section: &str,
    templates: &Templates,
    conf: &tini::Ini,
    filename: &str,
    output_dir: &Path,
    ctx: &RunContext,
) {
    // `previous_version` and `file_name` need to survive into the
    // error branch, so track them outside the Result.
    let mut previous_version: Option<String> = None;
    let mut file_name: Option<String> = None;

    let result = run_section_inner(
        section,
        templates,
        conf,
        filename,
        output_dir,
        ctx,
        &mut previous_version,
        &mut file_name,
    );

    let (updated, current_version) = match &result {
        Ok(outcome) => (
            outcome.updated(),
            outcome.current_version().map(String::from),
        ),
        Err(e) => {
            log_error_with_stack_trace(format!("{}", e));
            (false, None)
        }
    };

    ctx.reporter.emit(&OutputRecord {
        updated,
        tool_name: section.to_string(),
        file_name,
        previous_version,
        current_version,
    });
}

#[allow(clippy::too_many_arguments)]
fn run_section_inner(
    section: &str,
    templates: &Templates,
    conf: &tini::Ini,
    filename: &str,
    output_dir: &Path,
    ctx: &RunContext,
    previous_version: &mut Option<String>,
    file_name: &mut Option<String>,
) -> Result<Outcome> {
    let tmp = read_section_into_map(conf, section);
    let mut cf = Config::new();
    insert_fields_from_template(&mut cf, templates, &tmp)?;

    // First get the project - required
    match tmp.get("page_url") {
        Some(p) => cf.page_url = p.clone(),
        None => {
            if cf.page_url.is_empty() {
                warn!(
                    "[{}] Section {} is missing required field \
                     \"page_url\"",
                    section, section
                );
                return Ok(Outcome::NoHit);
            }
        }
    };
    debug!("[{}] Processing: {}", section, &cf.page_url);

    if let Some(value) = tmp.get("anchor_tag") {
        cf.anchor_tag = strfmt(value, &tmp)?;
    };

    if let Some(value) = tmp.get("anchor_text") {
        cf.anchor_text = strfmt(value, &tmp)?;
    };

    if let Some(value) = tmp.get("version_tag") {
        cf.version_tag = Some(strfmt(value, &tmp)?);
    };

    if let Some(value) = tmp.get("commit_tag") {
        cf.commit_tag = Some(strfmt(value, &tmp)?);
    };

    cf.extraction_targets = build_extraction_targets(section, &tmp)?;

    if let Some(value) = tmp.get("version") {
        cf.version = Some(strfmt(value, &tmp)?);
    };
    if let Some(value) = tmp.get("commit") {
        cf.commit = Some(strfmt(value, &tmp)?);
    };

    // Publish the two fields needed for the CSV row now that
    // substitutions are done. They remain accurate even on error paths
    // after this point.
    *previous_version = cf.version.clone();
    *file_name = file_name_for_report(&cf.extraction_targets);

    let outcome = process(section, &mut cf, output_dir)?;

    if let Outcome::Updated { version, commit } = &outcome {
        let _lock = ctx.config_write.lock().unwrap();
        let conf_write = tini::Ini::from_file(&filename).unwrap();
        if let Some(new_commit) = commit {
            info!(
                "[{}] Downloaded new version: {}:{}",
                section, &version, new_commit
            );
            conf_write
                .section(section)
                .item("version", version)
                .item("commit", new_commit)
                .to_file(&filename)
                .unwrap();
        } else {
            info!("[{}] Downloaded new version: {}", section, &version);
            conf_write
                .section(section)
                .item("version", version)
                .to_file(&filename)
                .unwrap();
        }
        debug!("[{}] Updated config file.", section);
    }
    Ok(outcome)
}

/// Build the in-memory plan for what to extract from the archive
/// (or, for non-archive downloads, where to write the single file).
///
/// Two INI keys feed in:
///   * `target_filename_to_extract_from_archive` — singular form, may
///     be a regex, paired with `desired_filename` for renaming.
///   * `target_filenames_to_extract_from_archive` — plural form, a
///     comma-separated list. Each entry is a regex (typically a
///     literal filename) and the file is extracted under its archive
///     basename. `desired_filename` does NOT apply here.
///
/// The two are mutually exclusive; specifying both, or specifying the
/// plural form together with `desired_filename`, is a config error.
/// If neither is given, default to a single target whose pattern and
/// destination are both the section name.
fn build_extraction_targets(
    section: &str,
    tmp: &HashMap<String, String>,
) -> Result<Vec<ExtractionTarget>> {
    let plural_raw = tmp.get("target_filenames_to_extract_from_archive");
    let singular_raw = tmp.get("target_filename_to_extract_from_archive");
    let desired_filename_raw = tmp.get("desired_filename");

    if plural_raw.is_some() && singular_raw.is_some() {
        return Err(anyhow!(
            "[{}] target_filename_to_extract_from_archive and \
             target_filenames_to_extract_from_archive are mutually exclusive",
            section
        ));
    }
    if plural_raw.is_some() && desired_filename_raw.is_some() {
        return Err(anyhow!(
            "[{}] desired_filename does not apply when \
             target_filenames_to_extract_from_archive is set; extracted \
             files keep their archive names",
            section
        ));
    }

    if let Some(raw) = plural_raw {
        // JSON array of strings, e.g. `["rg", "rg.1", "rg.bash"]`.
        // JSON's quoting/escaping handles items containing spaces or
        // commas without needing a custom split rule.
        let names: Vec<String> = serde_json::from_str(raw).map_err(|e| {
            anyhow!(
                "[{}] target_filenames_to_extract_from_archive must be a \
                 JSON array of strings, e.g. [\"rg\", \"rg.1\"]: {}",
                section,
                e
            )
        })?;
        if names.is_empty() {
            return Err(anyhow!(
                "[{}] target_filenames_to_extract_from_archive must list \
                 at least one filename",
                section
            ));
        }
        let mut targets = Vec::with_capacity(names.len());
        for item in &names {
            let pattern_str = strfmt(item, tmp)?;
            let pattern = regex::Regex::new(&format!("^{}$", &pattern_str))?;
            targets.push(ExtractionTarget {
                pattern_str,
                pattern,
                rename_to: None,
            });
        }
        Ok(targets)
    } else {
        let pattern_str = if let Some(value) = singular_raw {
            strfmt(value, tmp)?
        } else {
            section.to_owned()
        };
        let rename_to = if let Some(value) = desired_filename_raw {
            Some(strfmt(value, tmp)?)
        } else {
            Some(pattern_str.clone())
        };
        let pattern = regex::Regex::new(&format!("^{}$", &pattern_str))?;
        Ok(vec![ExtractionTarget {
            pattern_str,
            pattern,
            rename_to,
        }])
    }
}

/// Build the `file_name` field for the CSV reporter. Multiple targets
/// are joined with `;` so the row stays informative even in plural mode.
fn file_name_for_report(targets: &[ExtractionTarget]) -> Option<String> {
    if targets.is_empty() {
        None
    } else {
        Some(
            targets
                .iter()
                .map(|t| t.predicted_output_name())
                .collect::<Vec<_>>()
                .join(";"),
        )
    }
}

/// True when every target's predicted output already exists in
/// `output_dir` — i.e. there's nothing the up-to-date check needs to
/// refresh. In plural mode this is conservative: a single missing
/// file forces a re-download of all of them, which is fine since
/// they all come out of the same archive.
fn target_file_already_exists(conf: &Config, output_dir: &Path) -> bool {
    !conf.extraction_targets.is_empty()
        && conf
            .extraction_targets
            .iter()
            .all(|t| output_dir.join(t.predicted_output_name()).exists())
}

fn process(section: &str, conf: &mut Config, output_dir: &Path) -> Result<Outcome> {
    let url = &conf.page_url;

    let parse_result = match conf.method.as_str() {
        "api_json" => parse_json(section, conf, url)?,
        _ => parse_html_page(section, conf, url)?,
    };

    let hit = match parse_result {
        Some(hit) => hit,
        None => return Ok(Outcome::NoHit),
    };

    let existing_version = conf.version.as_ref().unwrap();
    // TODO: must compare each of the components of the version string as integers.
    if target_file_already_exists(conf, output_dir) && &hit.version < existing_version {
        debug!(
            "[{}] Found version is not newer: {}; Skipping.",
            section, &hit.version
        );
        return Ok(Outcome::UpToDate {
            version: hit.version,
        });
    } else if target_file_already_exists(conf, output_dir) && &hit.version == existing_version {
        // If a commit tag has been specified for this conf, we should check
        // that too. If the version tag is the same, and the commit hash
        // is merely different, we will also consider that as a new version.
        if let Some(ref found_commit) = hit.commit {
            debug!("Found commit: {found_commit}");
            if let Some(current_commit) = &conf.commit {
                debug!("Current commit: {current_commit}");
                if found_commit == current_commit {
                    debug!(
                        "[{}] Found version {}:{} is not newer than existing version {}:{}; Skipping.",
                        section, &hit.version, found_commit, &existing_version, current_commit
                    );
                    return Ok(Outcome::UpToDate {
                        version: hit.version,
                    });
                }
            }
            // Reaching here means there was no commit current in the config file.
            // We will continue with the download and the found_commit shiould
            // get written in.
        } else {
            // If the commit tag wasn't configured, nor found when
            // scanning, we treat that as the commit not being different.
            debug!(
                "[{}] Found version {} is not newer than existing version {}; Skipping.",
                section, &hit.version, &existing_version
            );
            return Ok(Outcome::UpToDate {
                version: hit.version,
            });
        };
    }

    info!("[{}] Downloading version {}", section, &hit.version);

    let download_url = &hit.download_url;
    let ext = {
        if vec![".tar.gz", ".tgz"]
            .iter()
            .any(|ext| download_url.ends_with(ext))
        {
            ".tar.gz"
        } else if download_url.ends_with(".gz") {
            ".gz"
        } else if vec![".tar.xz", ".txz"]
            .iter()
            .any(|ext| download_url.ends_with(ext))
        {
            ".tar.xz"
        } else if download_url.ends_with(".zip") {
            ".zip"
        } else if download_url.ends_with(".exe") {
            ".exe"
        } else if download_url.ends_with(".com") {
            ".com"
        } else if download_url.ends_with(".appimage") {
            ".appimage"
        } else if download_url.ends_with(".AppImage") {
            ".AppImage"
        } else if let Some(suffix) = slice_from_end(download_url, 8) {
            // Look at the last 8 chars of the url -> if there's no dot, that
            // probably means no file extension is present, which likely means that
            // the download is a binary.
            if !suffix.contains('.') {
                // This will be treated as the binary
                debug!(
                    "[{}] Returning empty string as ext {}",
                    section, &download_url
                );
                ""
            } else {
                warn!(
                    "[{}] Failed to match known file extensions. Skipping.",
                    section
                );
                return Ok(Outcome::ExtUnsupported {
                    version: hit.version,
                });
            }
        } else {
            warn!(
                "[{}] Failed to match known file extensions. Skipping.",
                section
            );
            return Ok(Outcome::ExtUnsupported {
                version: hit.version,
            });
        }
    };

    let resp = http_agent().get(download_url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
            .call()?;
    let mut reader = resp.into_body().into_reader();
    let mut buf: Vec<u8> = Vec::new();
    reader.read_to_end(&mut buf)?;

    let extracted: Vec<PathBuf> = if ext == ".tar.xz" {
        tarxzfile::extract_target_from_tarxz(&mut buf, conf, output_dir)
    } else if ext == ".zip" {
        zipfile::extract_target_from_zipfile(&mut buf, conf, output_dir)?
    } else if ext == ".tar.gz" {
        tarfile::extract_target_from_tarfile(&mut buf, conf, output_dir)
    } else if ext == ".gz" {
        gzfile::extract_target_from_gzfile(section, &buf, conf, output_dir)?
    } else if vec![".exe", "", ".com", ".appimage", ".AppImage"].contains(&ext) {
        // Single-file downloads (Windows executables, AppImages,
        // bare binaries) aren't archives — there's nothing to match
        // against. Only the singular form is meaningful here.
        let target = conf.singular_target().ok_or_else(|| {
            anyhow!(
                "[{}] target_filenames_to_extract_from_archive cannot be \
                 used with non-archive downloads (extension {:?})",
                section,
                ext
            )
        })?;
        let desired_filename = target.rename_to.as_ref().unwrap();
        let dest = output_dir.join(desired_filename);
        let tmp_dest = output_dir.join(format!("{}.tmp", desired_filename));

        let mut output = std::fs::File::create(&tmp_dest)?;
        info!(
            "[{}] Saving {} to {}",
            section,
            &download_url,
            dest.display()
        );
        output.write_all(&buf)?;
        std::fs::rename(&tmp_dest, &dest)?;
        vec![dest]
    } else {
        Vec::new()
    };

    if ext != ".exe" {
        for path in &extracted {
            set_executable(path)?;
        }
    }

    Ok(Outcome::Updated {
        version: hit.version,
        commit: hit.commit,
    })
}

/// Change file permissions to be executable. This only happens on
/// posix; on Windows it does nothing.
#[cfg(target_family = "unix")]
fn set_executable(path: &Path) -> Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    if perms.mode() & 0o111 == 0 {
        debug!(
            "File {} is not yet executable, setting bits.",
            path.display()
        );
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}
#[cfg(not(target_family = "unix"))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn parse_json(section: &str, conf: &Config, url: &str) -> Result<Option<Hit>> {
    let mut attempts_remaining = 10;
    let resp = loop {
        if attempts_remaining == 0 {
            return Err(anyhow!(format!("Failed to download {}", section)));
        } else {
            attempts_remaining -= 1;
        }

        let agent = http_agent();
        let resp = if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            let authorization_header_value = format!("token {token}");
            agent.get(url)
                    .header("Authorization", &authorization_header_value)
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
                    .call()
        } else {
            agent.get(url)
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
                .call()
        };

        let response = match resp {
            Ok(response) => response,
            Err(_) => {
                /* some kind of io/transport error */
                let msg = format!("Unexpected error fetching {url}.");
                return Err(anyhow!(msg));
            }
        };

        let status_code = response.status().as_u16();
        match status_code {
            200..=299 => break response,
            // https://developer.mozilla.org/en-US/docs/Web/HTTP/Status#client_error_responses
            403 | 408 | 425 | 429 | 500 | 502 | 503 | 504 => {
                let zzz = ((10 - attempts_remaining) * 4).min(60);
                if status_code == 403 {
                    let body = response.into_body().read_to_string()?;
                    info!("Got 403: {body}");
                }
                info!("Got status {status_code} fetching {section}. Sleeping for {zzz} secs...");
                std::thread::sleep(Duration::from_secs(zzz));
                continue;
            }
            _ => {
                let body = response.into_body().read_to_string()?;
                let msg = format!(
                    "Unexpected error fetching {url}. Status {status_code}. \
                    Body: {body}"
                );
                return Err(anyhow!(msg));
            }
        };
    };

    let body = resp.into_body().read_to_string()?;
    debug!("{}", &body);
    extract_data_from_json(body, conf)
}

fn extract_data_from_json<T: AsRef<str>>(payload: T, conf: &Config) -> Result<Option<Hit>> {
    // Extract from JSON
    use jsonpath_rust::JsonPath;
    use serde_json::Value;
    use std::str::FromStr;

    let data = Value::from_str(payload.as_ref())?;

    let vtag = conf.version_tag.clone().unwrap();
    let version_str = data
        .query(&vtag)?
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let commit_str = if let Some(ctag) = &conf.commit_tag {
        data.query(ctag)?
            .first()
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    let urls: Vec<String> = data
        .query(&conf.anchor_tag)?
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let re_pat = regex::Regex::new(&conf.anchor_text)?;

    for u in urls {
        if re_pat.is_match(&u) {
            return Ok(Some(Hit {
                version: version_str,
                commit: commit_str,
                download_url: u,
            }));
        }
    }

    Ok(None)
}

/// This function parses the target webpage trying to find two things:
/// 1. The download link for the target binary
/// 2. The version
///
/// To find the download link, we check all links on the page that
/// match the selector given in `anchor_tag`. There could be many
/// links (`<a>` tags) that match that anchor tag, and we'll keep
/// checking all of these until we find one whose "text" value
/// matches the regex given in the `anchor_text` field. This regex
/// should be a complete match.
fn parse_html_page(section: &str, conf: &Config, url: &str) -> Result<Option<Hit>> {
    debug!("[{}] Fetching page at {}", section, &url);

    // Retry with backoff
    let mut attempts_remaining = 10;
    let resp = loop {
        if attempts_remaining == 0 {
            return Err(anyhow!(format!("Failed to download {}", section)));
        } else {
            attempts_remaining -= 1;
        }

        let resp = http_agent().get(url)
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
                .call()?;
        let status_code = resp.status().as_u16();

        debug!("Fetching {section}, status: {status_code}");
        match status_code {
            200..=299 => break resp,
            // https://developer.mozilla.org/en-US/docs/Web/HTTP/Status#client_error_responses
            403 | 408 | 425 | 429 | 500 | 502 | 503 | 504 => {
                let zzz = ((10 - attempts_remaining) * 4).min(60);
                info!("Got status {status_code} fetching {section}. Sleeping for {zzz} secs...");
                std::thread::sleep(Duration::from_secs(zzz));
                continue;
            }
            _ => {
                let body = resp.into_body().read_to_string()?;
                let msg = format!(
                    "Unexpected error fetching {url}. Status {status_code}. \
                    Body: {body}"
                );
                return Err(anyhow!(msg));
            }
        };
    };

    let body = resp.into_body().read_to_string()?;
    debug!("{}", &body);

    debug!("[{}] Setting up parsers", section);
    let fragment = Html::parse_document(&body);
    let stories = match Selector::parse(&conf.anchor_tag) {
        Ok(s) => s,
        Err(e) => {
            warn!("[{}] Parser error at {}: {:?}", section, url, e);
            return Ok(None);
        }
    };
    let versions = Selector::parse(conf.version_tag.as_ref().unwrap()).unwrap();
    let re_pat = regex::Regex::new(format!("^{}$", &conf.anchor_text).as_str())?;

    debug!("[{}] Looking for matches...", section);
    for story in fragment.select(&stories) {
        if let Some(href) = &story.value().attr("href") {
            // This is the download target in the matched link
            let download_url = if href.starts_with("http") {
                // Absolute path
                href.to_string()
            } else if href.starts_with('/') || href.starts_with("../") {
                // Relative to domain
                let mut u = Url::parse(url)?;
                u.set_query(None);
                u.path_segments_mut().unwrap().clear();
                format!("{}", u.join(href)?)
            } else {
                // Relative to page url
                let mut u = Url::parse(url)?;
                u.set_query(None);
                //u.path_segments_mut().unwrap().clear();
                format!("{}", u.join(href)?)
            };

            debug!("[{}] possible download_url?: {}", section, &download_url);

            trace!("[{}] inner html: {:?}", section, &story.inner_html());
            let link_text = &story.text().collect::<Vec<_>>().join(" ");
            let link_text = link_text.trim();
            trace!("[{}] tag text: {}", section, link_text);
            if !re_pat.is_match(link_text) {
                continue;
            }
            debug!("[{}] Found a match for anchor_text: {}", section, link_text);

            return if let Some(raw_version) = fragment.select(&versions).next() {
                let version = raw_version.text().join("").trim().to_string();
                info!("[{}] Found a match on versions tag: {}", section, version);
                Ok(Some(Hit {
                    version,
                    // TODO: implement commit tracking for HTML page extraction
                    commit: None,
                    download_url,
                }))
            } else {
                warn!(
                    "[{}] Download link {} was found but failed to match version \
                     tag \"{}\"",
                    section,
                    &download_url,
                    conf.version_tag.as_ref().unwrap()
                );
                Ok(None)
            };
        }
    }
    warn!("[{}] Matched nothing at url {}", section, url);
    Ok(None)
}

/// Returns a slice of the last n characters of a string
fn slice_from_end(s: &str, n: usize) -> Option<&str> {
    s.char_indices().rev().nth(n).map(|(i, _)| &s[i..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_data_from_json() -> Result<()> {
        // This is the payload returned from the github API
        let payload = r##"
            {
              "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/44518686",
              "assets_url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/44518686/assets",
              "upload_url": "https://uploads.github.com/repos/BurntSushi/ripgrep/releases/44518686/assets{?name,label}",
              "html_url": "https://github.com/BurntSushi/ripgrep/releases/tag/13.0.0",
              "id": 44518686,
              "author": {
                "login": "github-actions[bot]",
                "id": 41898282,
                "node_id": "MDM6Qm90NDE4OTgyODI=",
                "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                "gravatar_id": "",
                "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                "html_url": "https://github.com/apps/github-actions",
                "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                "type": "Bot",
                "site_admin": false
              },
              "node_id": "MDc6UmVsZWFzZTQ0NTE4Njg2",
              "tag_name": "13.0.0",
              "target_commitish": "af6b6c543b224d348a8876f0c06245d9ea7929c5",
              "name": "13.0.0",
              "draft": false,
              "prerelease": false,
              "created_at": "2021-06-12T12:12:24Z",
              "published_at": "2021-06-12T12:27:16Z",
              "assets": [
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38486868",
                  "id": 38486868,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDg2ODY4",
                  "name": "ripgrep-13.0.0-arm-unknown-linux-gnueabihf.tar.gz",
                  "label": "",
                  "uploader": {
                    "login": "github-actions[bot]",
                    "id": 41898282,
                    "node_id": "MDM6Qm90NDE4OTgyODI=",
                    "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                    "html_url": "https://github.com/apps/github-actions",
                    "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                    "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                    "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                    "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                    "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                    "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                    "type": "Bot",
                    "site_admin": false
                  },
                  "content_type": "application/octet-stream",
                  "state": "uploaded",
                  "size": 1763861,
                  "download_count": 4161,
                  "created_at": "2021-06-12T12:31:57Z",
                  "updated_at": "2021-06-12T12:31:58Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-arm-unknown-linux-gnueabihf.tar.gz"
                },
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38486879",
                  "id": 38486879,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDg2ODc5",
                  "name": "ripgrep-13.0.0-i686-pc-windows-msvc.zip",
                  "label": "",
                  "uploader": {
                    "login": "github-actions[bot]",
                    "id": 41898282,
                    "node_id": "MDM6Qm90NDE4OTgyODI=",
                    "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                    "html_url": "https://github.com/apps/github-actions",
                    "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                    "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                    "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                    "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                    "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                    "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                    "type": "Bot",
                    "site_admin": false
                  },
                  "content_type": "application/octet-stream",
                  "state": "uploaded",
                  "size": 1591463,
                  "download_count": 3787,
                  "created_at": "2021-06-12T12:32:47Z",
                  "updated_at": "2021-06-12T12:32:48Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-i686-pc-windows-msvc.zip"
                },
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38486907",
                  "id": 38486907,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDg2OTA3",
                  "name": "ripgrep-13.0.0-x86_64-apple-darwin.tar.gz",
                  "label": "",
                  "uploader": {
                    "login": "github-actions[bot]",
                    "id": 41898282,
                    "node_id": "MDM6Qm90NDE4OTgyODI=",
                    "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                    "html_url": "https://github.com/apps/github-actions",
                    "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                    "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                    "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                    "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                    "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                    "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                    "type": "Bot",
                    "site_admin": false
                  },
                  "content_type": "application/octet-stream",
                  "state": "uploaded",
                  "size": 1815615,
                  "download_count": 214176,
                  "created_at": "2021-06-12T12:35:00Z",
                  "updated_at": "2021-06-12T12:35:00Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-apple-darwin.tar.gz"
                },
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38486889",
                  "id": 38486889,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDg2ODg5",
                  "name": "ripgrep-13.0.0-x86_64-pc-windows-gnu.zip",
                  "label": "",
                  "uploader": {
                    "login": "github-actions[bot]",
                    "id": 41898282,
                    "node_id": "MDM6Qm90NDE4OTgyODI=",
                    "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                    "html_url": "https://github.com/apps/github-actions",
                    "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                    "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                    "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                    "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                    "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                    "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                    "type": "Bot",
                    "site_admin": false
                  },
                  "content_type": "application/octet-stream",
                  "state": "uploaded",
                  "size": 9811405,
                  "download_count": 11733,
                  "created_at": "2021-06-12T12:33:25Z",
                  "updated_at": "2021-06-12T12:33:26Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-pc-windows-gnu.zip"
                },
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38486875",
                  "id": 38486875,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDg2ODc1",
                  "name": "ripgrep-13.0.0-x86_64-pc-windows-msvc.zip",
                  "label": "",
                  "uploader": {
                    "login": "github-actions[bot]",
                    "id": 41898282,
                    "node_id": "MDM6Qm90NDE4OTgyODI=",
                    "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                    "html_url": "https://github.com/apps/github-actions",
                    "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                    "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                    "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                    "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                    "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                    "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                    "type": "Bot",
                    "site_admin": false
                  },
                  "content_type": "application/octet-stream",
                  "state": "uploaded",
                  "size": 1734357,
                  "download_count": 34593,
                  "created_at": "2021-06-12T12:32:30Z",
                  "updated_at": "2021-06-12T12:32:30Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-pc-windows-msvc.zip"
                },
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38486871",
                  "id": 38486871,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDg2ODcx",
                  "name": "ripgrep-13.0.0-x86_64-unknown-linux-musl.tar.gz",
                  "label": "",
                  "uploader": {
                    "login": "github-actions[bot]",
                    "id": 41898282,
                    "node_id": "MDM6Qm90NDE4OTgyODI=",
                    "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/github-actions%5Bbot%5D",
                    "html_url": "https://github.com/apps/github-actions",
                    "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
                    "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
                    "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
                    "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
                    "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
                    "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
                    "type": "Bot",
                    "site_admin": false
                  },
                  "content_type": "application/octet-stream",
                  "state": "uploaded",
                  "size": 2109801,
                  "download_count": 302481,
                  "created_at": "2021-06-12T12:32:02Z",
                  "updated_at": "2021-06-12T12:32:03Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-unknown-linux-musl.tar.gz"
                },
                {
                  "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/assets/38493219",
                  "id": 38493219,
                  "node_id": "MDEyOlJlbGVhc2VBc3NldDM4NDkzMjE5",
                  "name": "ripgrep_13.0.0_amd64.deb",
                  "label": null,
                  "uploader": {
                    "login": "BurntSushi",
                    "id": 456674,
                    "node_id": "MDQ6VXNlcjQ1NjY3NA==",
                    "avatar_url": "https://avatars.githubusercontent.com/u/456674?v=4",
                    "gravatar_id": "",
                    "url": "https://api.github.com/users/BurntSushi",
                    "html_url": "https://github.com/BurntSushi",
                    "followers_url": "https://api.github.com/users/BurntSushi/followers",
                    "following_url": "https://api.github.com/users/BurntSushi/following{/other_user}",
                    "gists_url": "https://api.github.com/users/BurntSushi/gists{/gist_id}",
                    "starred_url": "https://api.github.com/users/BurntSushi/starred{/owner}{/repo}",
                    "subscriptions_url": "https://api.github.com/users/BurntSushi/subscriptions",
                    "organizations_url": "https://api.github.com/users/BurntSushi/orgs",
                    "repos_url": "https://api.github.com/users/BurntSushi/repos",
                    "events_url": "https://api.github.com/users/BurntSushi/events{/privacy}",
                    "received_events_url": "https://api.github.com/users/BurntSushi/received_events",
                    "type": "User",
                    "site_admin": false
                  },
                  "content_type": "application/vnd.debian.binary-package",
                  "state": "uploaded",
                  "size": 1574096,
                  "download_count": 84528,
                  "created_at": "2021-06-12T17:36:20Z",
                  "updated_at": "2021-06-12T17:36:21Z",
                  "browser_download_url": "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep_13.0.0_amd64.deb"
                }
              ],
              "tarball_url": "https://api.github.com/repos/BurntSushi/ripgrep/tarball/13.0.0",
              "zipball_url": "https://api.github.com/repos/BurntSushi/ripgrep/zipball/13.0.0",
              "body": "ripgrep 13 is a new major version release of ripgrep that primarily contains\r\nbug fixes, some performance improvements and a few minor breaking changes.\r\nThere is also a fix for a security vulnerability on Windows\r\n([CVE-2021-3013](https://cve.mitre.org/cgi-bin/cvename.cgi?name=CVE-2021-3013)).\r\n\r\nIn case you haven't heard of it before, ripgrep is a line-oriented search\r\ntool that recursively searches the current directory for a regex pattern. By\r\ndefault, ripgrep will respect gitignore rules and automatically skip hidden\r\nfiles/directories and binary files.\r\n\r\nSome highlights:\r\n\r\nA new short flag, `-.`, has been added. It is an alias for the `--hidden` flag,\r\nwhich instructs ripgrep to search hidden files and directories.\r\n\r\nripgrep is now using a new\r\n[vectorized implementation of `memmem`](https://github.com/BurntSushi/memchr/pull/82),\r\nwhich accelerates many common searches. If you notice any performance\r\nregressions (or major improvements), I'd love to hear about them through an\r\nissue report!\r\n\r\nAlso, for Windows users targeting MSVC, Cargo will now build fully static\r\nexecutables of ripgrep. The release binaries for ripgrep 13 have been compiled\r\nusing this configuration.\r\n\r\n**BREAKING CHANGES**:\r\n\r\n**Binary detection output has changed slightly.**\r\n\r\nIn this release, a small tweak has been made to the output format when a binary\r\nfile is detected. Previously, it looked like this:\r\n\r\n```\r\nBinary file FOO matches (found \"\\0\" byte around offset XXX)\r\n```\r\n\r\nNow it looks like this:\r\n\r\n```\r\nFOO: binary file matches (found \"\\0\" byte around offset XXX)\r\n```\r\n\r\n**vimgrep output in multi-line now only prints the first line for each match.**\r\n\r\nSee [issue 1866](https://github.com/BurntSushi/ripgrep/issues/1866) for more\r\ndiscussion on this. Previously, every line in a match was duplicated, even\r\nwhen it spanned multiple lines. There are no changes to vimgrep output when\r\nmulti-line mode is disabled.\r\n\r\n**In multi-line mode, --count is now equivalent to --count-matches.**\r\n\r\nThis appears to match how `pcre2grep` implements `--count`. Previously, ripgrep\r\nwould produce outright incorrect counts. Another alternative would be to simply\r\ncount the number of lines---even if it's more than the number of matches---but\r\nthat seems highly unintuitive.\r\n\r\n**FULL LIST OF FIXES AND IMPROVEMENTS:**\r\n\r\nSecurity fixes:\r\n\r\n* [CVE-2021-3013](https://cve.mitre.org/cgi-bin/cvename.cgi?name=CVE-2021-3013):\r\n  Fixes a security hole on Windows where running ripgrep with either the\r\n  `-z/--search-zip` or `--pre` flags can result in running arbitrary\r\n  executables from the current directory.\r\n* [VULN #1773](https://github.com/BurntSushi/ripgrep/issues/1773):\r\n  This is the public facing issue tracking CVE-2021-3013. ripgrep's README\r\n  now contains a section describing how to report a vulnerability.\r\n\r\nPerformance improvements:\r\n\r\n* [PERF #1657](https://github.com/BurntSushi/ripgrep/discussions/1657):\r\n  Check if a file should be ignored first before issuing stat calls.\r\n* [PERF memchr#82](https://github.com/BurntSushi/memchr/pull/82):\r\n  ripgrep now uses a new vectorized implementation of `memmem`.\r\n\r\nFeature enhancements:\r\n\r\n* Added or improved file type filtering for ASP, Bazel, dvc, FlatBuffers,\r\n  Futhark, minified files, Mint, pofiles (from GNU gettext) Racket, Red, Ruby,\r\n  VCL, Yang.\r\n* [FEATURE #1404](https://github.com/BurntSushi/ripgrep/pull/1404):\r\n  ripgrep now prints a warning if nothing is searched.\r\n* [FEATURE #1613](https://github.com/BurntSushi/ripgrep/pull/1613):\r\n  Cargo will now produce static executables on Windows when using MSVC.\r\n* [FEATURE #1680](https://github.com/BurntSushi/ripgrep/pull/1680):\r\n  Add `-.` as a short flag alias for `--hidden`.\r\n* [FEATURE #1842](https://github.com/BurntSushi/ripgrep/issues/1842):\r\n  Add `--field-{context,match}-separator` for customizing field delimiters.\r\n* [FEATURE #1856](https://github.com/BurntSushi/ripgrep/pull/1856):\r\n  The README now links to a\r\n  [Spanish translation](https://github.com/UltiRequiem/traducciones/tree/master/ripgrep).\r\n\r\nBug fixes:\r\n\r\n* [BUG #1277](https://github.com/BurntSushi/ripgrep/issues/1277):\r\n  Document cygwin path translation behavior in the FAQ.\r\n* [BUG #1739](https://github.com/BurntSushi/ripgrep/issues/1739):\r\n  Fix bug where replacements were buggy if the regex matched a line terminator.\r\n* [BUG #1311](https://github.com/BurntSushi/ripgrep/issues/1311):\r\n  Fix multi-line bug where a search & replace for `\\n` didn't work as expected.\r\n* [BUG #1401](https://github.com/BurntSushi/ripgrep/issues/1401):\r\n  Fix buggy interaction between PCRE2 look-around and `-o/--only-matching`.\r\n* [BUG #1412](https://github.com/BurntSushi/ripgrep/issues/1412):\r\n  Fix multi-line bug with searches using look-around past matching lines.\r\n* [BUG #1577](https://github.com/BurntSushi/ripgrep/issues/1577):\r\n  Fish shell completions will continue to be auto-generated.\r\n* [BUG #1642](https://github.com/BurntSushi/ripgrep/issues/1642):\r\n  Fixes a bug where using `-m` and `-A` printed more matches than the limit.\r\n* [BUG #1703](https://github.com/BurntSushi/ripgrep/issues/1703):\r\n  Clarify the function of `-u/--unrestricted`.\r\n* [BUG #1708](https://github.com/BurntSushi/ripgrep/issues/1708):\r\n  Clarify how `-S/--smart-case` works.\r\n* [BUG #1730](https://github.com/BurntSushi/ripgrep/issues/1730):\r\n  Clarify that CLI invocation must always be valid, regardless of config file.\r\n* [BUG #1741](https://github.com/BurntSushi/ripgrep/issues/1741):\r\n  Fix stdin detection when using PowerShell in UNIX environments.\r\n* [BUG #1756](https://github.com/BurntSushi/ripgrep/pull/1756):\r\n  Fix bug where `foo/**` would match `foo`, but it shouldn't.\r\n* [BUG #1765](https://github.com/BurntSushi/ripgrep/issues/1765):\r\n  Fix panic when `--crlf` is used in some cases.\r\n* [BUG #1638](https://github.com/BurntSushi/ripgrep/issues/1638):\r\n  Correctly sniff UTF-8 and do transcoding, like we do for UTF-16.\r\n* [BUG #1816](https://github.com/BurntSushi/ripgrep/issues/1816):\r\n  Add documentation for glob alternate syntax, e.g., `{a,b,..}`.\r\n* [BUG #1847](https://github.com/BurntSushi/ripgrep/issues/1847):\r\n  Clarify how the `--hidden` flag works.\r\n* [BUG #1866](https://github.com/BurntSushi/ripgrep/issues/1866#issuecomment-841635553):\r\n  Fix bug when computing column numbers in `--vimgrep` mode.\r\n* [BUG #1868](https://github.com/BurntSushi/ripgrep/issues/1868):\r\n  Fix bug where `--passthru` and `-A/-B/-C` did not override each other.\r\n* [BUG #1869](https://github.com/BurntSushi/ripgrep/pull/1869):\r\n  Clarify docs for `--files-with-matches` and `--files-without-match`.\r\n* [BUG #1878](https://github.com/BurntSushi/ripgrep/issues/1878):\r\n  Fix bug where `\\A` could produce unanchored matches in multiline search.\r\n* [BUG 94e4b8e3](https://github.com/BurntSushi/ripgrep/commit/94e4b8e3):\r\n  Fix column numbers with `--vimgrep` is used with `-U/--multiline`.",
              "reactions": {
                "url": "https://api.github.com/repos/BurntSushi/ripgrep/releases/44518686/reactions",
                "total_count": 359,
                "+1": 166,
                "-1": 0,
                "laugh": 3,
                "hooray": 79,
                "confused": 0,
                "heart": 71,
                "rocket": 35,
                "eyes": 5
              }
            }"##;
        /*
        This is the configuration in lifter.config for an entry:

        [template:github_api_latest]
        method = api_json
        # or method = scrape
        page_url = https://api.github.com/repos/{project}/releases/latest
        version_tag = $.tag_name
        anchor_tag = $.assets.*.browser_download_url

        [ripgrep-api]
        template = github_api_latest
        project = BurntSushi/ripgrep
        anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
        target_filename_to_extract_from_archive = rg
        version = 13.0.0

        After all substitutions are made, it will look like this:

        [ripgrep-api]
        method = api_json
        page_url = https://api.github.com/repos/BurntSushi/ripgrep/releases/latest
        version_tag = $.tag_name
        anchor_tag = $.assets.*.browser_download_url
        template = github_api_latest
        project = BurntSushi/ripgrep
        anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
        target_filename_to_extract_from_archive = rg
        version = 13.0.0
        */
        // Only the fields actually read by `extract_data_from_json`
        // need to be set; the rest fall through to `Default` so
        // future `Config` fields don't break this test.
        let conf = Config {
            anchor_tag: "$.assets.*.browser_download_url".to_string(),
            anchor_text: r"ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz".to_string(),
            version_tag: Some("$.tag_name".to_string()),
            ..Default::default()
        };
        let out = extract_data_from_json(payload, &conf)?;
        let expected_hit = Hit {
            version : "13.0.0".to_string(),
            download_url : "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-unknown-linux-musl.tar.gz".to_string(),
            commit: None,
        };
        assert_eq!(out, Some(expected_hit));
        Ok(())
    }

    fn ini_map<const N: usize>(pairs: [(&str, &str); N]) -> HashMap<String, String> {
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn singular_target_defaults_to_section_name() {
        let targets = build_extraction_targets("ripgrep", &ini_map([])).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].pattern_str, "ripgrep");
        assert_eq!(targets[0].rename_to.as_deref(), Some("ripgrep"));
    }

    #[test]
    fn singular_target_respects_desired_filename() {
        let tmp = ini_map([
            ("target_filename_to_extract_from_archive", "fcp-1.0-linux"),
            ("desired_filename", "fcp"),
        ]);
        let targets = build_extraction_targets("fcp", &tmp).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].pattern_str, "fcp-1.0-linux");
        assert_eq!(targets[0].rename_to.as_deref(), Some("fcp"));
    }

    #[test]
    fn plural_targets_keep_original_names() {
        let tmp = ini_map([(
            "target_filenames_to_extract_from_archive",
            r#"["rg", "doc/rg.1", "complete/rg.bash"]"#,
        )]);
        let targets = build_extraction_targets("ripgrep", &tmp).unwrap();
        let names: Vec<&str> = targets.iter().map(|t| t.pattern_str.as_str()).collect();
        assert_eq!(names, vec!["rg", "doc/rg.1", "complete/rg.bash"]);
        assert!(targets.iter().all(|t| t.rename_to.is_none()));
    }

    #[test]
    fn plural_handles_filenames_with_spaces_and_commas() {
        let tmp = ini_map([(
            "target_filenames_to_extract_from_archive",
            r#"["My Tool", "weird,name.txt"]"#,
        )]);
        let targets = build_extraction_targets("tool", &tmp).unwrap();
        let names: Vec<&str> = targets.iter().map(|t| t.pattern_str.as_str()).collect();
        assert_eq!(names, vec!["My Tool", "weird,name.txt"]);
    }

    #[test]
    fn plural_and_singular_are_mutually_exclusive() {
        let tmp = ini_map([
            ("target_filename_to_extract_from_archive", "rg"),
            (
                "target_filenames_to_extract_from_archive",
                r#"["rg", "rg.1"]"#,
            ),
        ]);
        assert!(build_extraction_targets("ripgrep", &tmp).is_err());
    }

    #[test]
    fn plural_with_desired_filename_errors() {
        let tmp = ini_map([
            (
                "target_filenames_to_extract_from_archive",
                r#"["rg", "rg.1"]"#,
            ),
            ("desired_filename", "ripgrep"),
        ]);
        assert!(build_extraction_targets("ripgrep", &tmp).is_err());
    }

    #[test]
    fn empty_plural_list_errors() {
        let tmp = ini_map([("target_filenames_to_extract_from_archive", "[]")]);
        assert!(build_extraction_targets("ripgrep", &tmp).is_err());
    }

    #[test]
    fn malformed_plural_list_errors() {
        let tmp = ini_map([("target_filenames_to_extract_from_archive", "rg, rg.1")]);
        let err = build_extraction_targets("ripgrep", &tmp).unwrap_err();
        assert!(
            format!("{}", err).contains("JSON array"),
            "expected JSON-array hint in error, got: {}",
            err
        );
    }

    #[test]
    fn singular_target_has_singular_target_helper() {
        let mut conf = Config::new();
        conf.extraction_targets = build_extraction_targets("rg", &ini_map([])).unwrap();
        assert!(conf.singular_target().is_some());
    }

    #[test]
    fn plural_targets_has_no_singular_target_helper() {
        let tmp = ini_map([(
            "target_filenames_to_extract_from_archive",
            r#"["rg", "rg.1"]"#,
        )]);
        let mut conf = Config::new();
        conf.extraction_targets = build_extraction_targets("ripgrep", &tmp).unwrap();
        assert!(conf.singular_target().is_none());
    }

    #[test]
    fn report_field_joins_predicted_names_with_semicolons() {
        let tmp = ini_map([(
            "target_filenames_to_extract_from_archive",
            r#"["rg", "doc/rg.1"]"#,
        )]);
        let targets = build_extraction_targets("ripgrep", &tmp).unwrap();
        assert_eq!(
            file_name_for_report(&targets).as_deref(),
            Some("rg;doc/rg.1")
        );
    }

    // -------- archive-layer integration tests --------
    //
    // Extractors take an explicit `output_dir`, so each test just
    // creates its own tempdir and passes it in. No process-wide CWD
    // gymnastics needed.

    fn build_test_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_path(name).unwrap();
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn build_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    fn make_conf_from_ini(section: &str, pairs: &[(&str, &str)]) -> Config {
        let tmp: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut conf = Config::new();
        conf.extraction_targets = build_extraction_targets(section, &tmp).unwrap();
        conf
    }

    #[test]
    fn extract_targets_from_tar_pulls_each_listed_file_under_original_basename() {
        let dir = tempfile::tempdir().unwrap();
        let tar_bytes = build_test_tar(&[
            ("rg", b"binary"),
            ("doc/rg.1", b"manpage"),
            ("complete/rg.bash", b"completion"),
            ("README.md", b"unwanted"),
        ]);

        let conf = make_conf_from_ini(
            "ripgrep",
            &[(
                "target_filenames_to_extract_from_archive",
                r#"["rg", "rg.1", "rg.bash"]"#,
            )],
        );

        let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
        let written = crate::archive::extract_targets_from_tar(&mut archive, &conf, dir.path());

        let written_set: std::collections::HashSet<PathBuf> = written.into_iter().collect();
        assert_eq!(written_set.len(), 3);
        assert!(written_set.contains(&dir.path().join("rg")));
        assert!(written_set.contains(&dir.path().join("rg.1")));
        assert!(written_set.contains(&dir.path().join("rg.bash")));

        assert_eq!(std::fs::read(dir.path().join("rg")).unwrap(), b"binary");
        assert_eq!(std::fs::read(dir.path().join("rg.1")).unwrap(), b"manpage");
        assert_eq!(
            std::fs::read(dir.path().join("rg.bash")).unwrap(),
            b"completion"
        );
        assert!(!dir.path().join("README.md").exists());
    }

    #[test]
    fn extract_targets_from_tar_renames_in_singular_mode() {
        let dir = tempfile::tempdir().unwrap();
        let tar_bytes = build_test_tar(&[("fcp-1.0-x86_64-unknown-linux-gnu", b"binary")]);

        let conf = make_conf_from_ini(
            "fcp",
            &[
                (
                    "target_filename_to_extract_from_archive",
                    "fcp-1.0-x86_64-unknown-linux-gnu",
                ),
                ("desired_filename", "fcp"),
            ],
        );

        let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
        let written = crate::archive::extract_targets_from_tar(&mut archive, &conf, dir.path());

        assert_eq!(written, vec![dir.path().join("fcp")]);
        assert_eq!(std::fs::read(dir.path().join("fcp")).unwrap(), b"binary");
        assert!(!dir.path().join("fcp-1.0-x86_64-unknown-linux-gnu").exists());
    }

    #[test]
    fn extract_targets_from_tar_warns_but_succeeds_when_a_target_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let tar_bytes = build_test_tar(&[("rg", b"binary")]);
        let conf = make_conf_from_ini(
            "ripgrep",
            &[(
                "target_filenames_to_extract_from_archive",
                r#"["rg", "missing-from-archive"]"#,
            )],
        );

        let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
        let written = crate::archive::extract_targets_from_tar(&mut archive, &conf, dir.path());

        // Only the matching target produces output; the missing one
        // is logged but doesn't fail the run.
        assert_eq!(written, vec![dir.path().join("rg")]);
        assert_eq!(std::fs::read(dir.path().join("rg")).unwrap(), b"binary");
    }

    #[test]
    fn extract_target_from_zipfile_pulls_each_listed_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip_bytes = build_test_zip(&[
            ("rg", b"binary"),
            ("doc/rg.1", b"manpage"),
            ("complete/rg.bash", b"completion"),
            ("README.md", b"unwanted"),
        ]);

        let conf = make_conf_from_ini(
            "ripgrep",
            &[(
                "target_filenames_to_extract_from_archive",
                r#"["rg", "rg.1", "rg.bash"]"#,
            )],
        );

        let written =
            crate::zipfile::extract_target_from_zipfile(&mut zip_bytes, &conf, dir.path()).unwrap();

        let written_set: std::collections::HashSet<PathBuf> = written.into_iter().collect();
        assert_eq!(written_set.len(), 3);
        assert!(written_set.contains(&dir.path().join("rg")));
        assert!(written_set.contains(&dir.path().join("rg.1")));
        assert!(written_set.contains(&dir.path().join("rg.bash")));

        assert_eq!(std::fs::read(dir.path().join("rg")).unwrap(), b"binary");
        assert_eq!(std::fs::read(dir.path().join("rg.1")).unwrap(), b"manpage");
        assert_eq!(
            std::fs::read(dir.path().join("rg.bash")).unwrap(),
            b"completion"
        );
        assert!(!dir.path().join("README.md").exists());
    }
}
