use std::collections::HashMap;
use std::io::{Read, Seek, Write};
#[cfg(target_family = "unix")]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use itertools::Itertools;
use log::*;
use scraper::{Html, Selector};
use strfmt::strfmt;
use url::Url;

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
    /// Target filename inside archive. Leave blank if download is not an archive.
    target_filename_to_extract_from_archive: Option<String>,
    /// After download/extraction, rename file to this
    desired_filename: Option<String>,
}

impl Config {
    fn new() -> Config {
        Config {
            ..Default::default()
        }
    }
}

#[derive(Debug, PartialEq)]
struct Hit {
    version: String,
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
        if let Some(value) = template_fields.get("method") {
            cf.method = strfmt(value, values)?;
        };
    };

    debug!("Substitutions complete: {:?}", &cf);
    Ok(())
}

pub fn run_section(
    section: &str,
    templates: &Templates,
    conf: &tini::Ini,
    filename: &str,
) -> Result<()> {
    let tmp = read_section_into_map(conf, section);
    let mut cf = Config::new();
    insert_fields_from_template(&mut cf, templates, &tmp)?;

    // First get the project - required
    match tmp.get("page_url") {
        Some(p) => cf.page_url = p.clone(),
        None => {
            if cf.page_url.is_empty() {
                return {
                    warn!(
                        "[{}] Section {} is missing required field \
                         \"page_url\"",
                        section, section
                    );
                    Ok(())
                };
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

    cf.target_filename_to_extract_from_archive =
        if let Some(value) = tmp.get("target_filename_to_extract_from_archive") {
            // The target filename parameter is present, so we'll use
            // that. However, it must also be substituted with the
            // current vars, in case it contains any template params.
            Some(strfmt(value, &tmp)?)
        } else {
            // The default filename to look for inside the archive
            // is the name of the section. For example, if the name
            // of the section in the config file is `[ripgrep]` then
            // this is what we'll search for. If instead the filename
            // inside the archive is `[rg]`, then that will be searched
            // for.
            Some(section.to_owned())
        };

    if let Some(value) = tmp.get("version") {
        cf.version = Some(strfmt(value, &tmp)?);
    };

    cf.desired_filename = if let Some(value) = tmp.get("desired_filename") {
        Some(strfmt(value, &tmp)?)
    } else {
        // No explicit "desired_filename" given, so we'll use the
        // target filename as the desired filename. Note that in the
        // case where no explicit target filename was given, we defaulted
        // to the section name. So that will flow on to here too.
        cf.target_filename_to_extract_from_archive.clone()
    };

    // Finally time to actually do some processing. Here we call
    // out to a function, and if we get something back, it means
    // we found and processed a new version. This section will
    // then update the config file with the new version.
    // TODO: would be useful to collect things that changed,
    //   and what versions they changed from/to.
    if let Some(new_version) = process(section, &mut cf)? {
        // New version, must update the version number in the
        // config file.
        info!("[{}] Downloaded new version: {}", section, &new_version);
        // TODO: actually need a mutex around the following 3 lines.
        let conf_write = tini::Ini::from_file(&filename).unwrap();
        conf_write
            .section(section)
            .item("version", &new_version)
            .to_file(&filename)
            .unwrap();
        debug!("[{}] Updated config file.", section);
    }
    Ok(())
}

fn target_file_already_exists(conf: &Config) -> bool {
    let filename_to_check = if let Some(fname) = conf.desired_filename.as_ref() {
        fname
    } else if let Some(fname) = conf.target_filename_to_extract_from_archive.as_ref() {
        fname
    } else {
        panic!("This should be impossible")
    };

    Path::new(&filename_to_check).exists()
}

fn process(section: &str, conf: &mut Config) -> Result<Option<String>> {
    let url = &conf.page_url;

    let parse_result = match conf.method.as_str() {
        "api_json" => parse_json(section, conf, url)?,
        _ => parse_html_page(section, conf, url)?,
    };

    let hit = match parse_result {
        Some(hit) => hit,
        None => return Ok(None),
    };

    let existing_version = conf.version.as_ref().unwrap();
    // TODO: must compare each of the components of the version string as integers.
    if target_file_already_exists(conf) && &hit.version <= existing_version {
        info!(
            "[{}] Found version is not newer: {}; Skipping.",
            section, &hit.version
        );
        return Ok(None);
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
                return Ok(None);
            }
        } else {
            warn!(
                "[{}] Failed to match known file extensions. Skipping.",
                section
            );
            return Ok(None);
        }
    };

    let resp = ureq::get(download_url)
            .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
            .call()?;
    let mut reader = resp.into_reader();
    let mut buf: Vec<u8> = Vec::new();
    reader.read_to_end(&mut buf)?;

    if ext == ".tar.xz" {
        extract_target_from_tarxz(&mut buf, conf);
    } else if ext == ".zip" {
        extract_target_from_zipfile(&mut buf, conf)?;
    } else if ext == ".tar.gz" {
        extract_target_from_tarfile(&mut buf, conf);
    } else if ext == ".gz" {
        extract_target_from_gzfile(&mut buf, conf);
    } else if vec![".exe", "", ".com", ".appimage", ".AppImage"].contains(&ext) {
        // Windows executables are not compressed, so we only need to
        // handle renames, if the option is given.
        // let fname = conf.desired_filename.clone().unwrap();
        // let mut od = output_dir.clone();
        // let outfilename = od.push(std::path::Path::new(&fname));
        let desired_filename = conf.desired_filename.as_ref().unwrap();
        let mut output = std::fs::File::create(&desired_filename)?;
        info!(
            "[{}] Saving {} to {}",
            section, &download_url, desired_filename
        );
        output.write_all(&buf)?;
    };

    if let Some(filename) = &conf.desired_filename {
        if ext != ".exe" {
            // TODO: this must be updated to handle output_dir
            set_executable(filename)?;
        }
    }

    Ok(Some(hit.version))
}

/// Change file permissions to be executable. This only happens on
/// posix; on Windows it does nothing.
#[cfg(target_family = "unix")]
fn set_executable(filename: &str) -> Result<()> {
    let mut perms = std::fs::metadata(&filename)?.permissions();
    if perms.mode() & 0o111 == 0 {
        debug!("File {} is not yet executable, setting bits.", filename);
        perms.set_mode(0o755);
        std::fs::set_permissions(&filename, perms)?;
    }
    Ok(())
}
#[cfg(not(target_family = "unix"))]
fn set_executable(filename: &str) -> Result<()> {
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

        let resp = ureq::get(url)
                .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
                .call()?;
        let status_code = resp.status();

        debug!("Fetching {section}, status: {status_code}");
        match status_code {
            200..=299 => break resp,
            // https://developer.mozilla.org/en-US/docs/Web/HTTP/Status#client_error_responses
            408 | 425 | 429 | 500 | 502 | 503 | 504 => {
                let zzz = ((10 - attempts_remaining) * 4).min(60);
                info!("Got status {status_code} fetching {section}. Sleeping for {zzz} secs...");
                std::thread::sleep(Duration::from_secs(zzz));
                continue;
            }
            _ => {
                // let body = resp.text()?;
                let body = resp.into_string()?;
                let msg = format!(
                    "Unexpected error fetching {url}. Status {status_code}. \
                    Body: {body}"
                );
                return Err(anyhow!(msg));
            }
        };
    };

    // let body = resp.text()?;
    let body = resp.into_string()?;
    debug!("{}", &body);
    extract_data_from_json(body, conf)
}

fn extract_data_from_json<T: AsRef<str>>(payload: T, conf: &Config) -> Result<Option<Hit>> {
    // Extract from JSON
    use jsonpath_rust::JsonPathFinder;

    let vtag = conf.version_tag.clone().unwrap();
    let finder = JsonPathFinder::from_str(
        payload.as_ref(),
        &vtag,
        // "$.first.second[?(@.active)]",
    )
    .unwrap();
    let item = &finder.find_slice()[0];
    let item = item.clone().to_data();
    let version_str = item.as_str().unwrap_or("");

    let finder = JsonPathFinder::from_str(
        payload.as_ref(),
        &conf.anchor_tag,
        // "$.first.second[?(@.active)]",
    )
    .unwrap();
    let urls = finder
        .find_slice()
        .iter()
        .map(|v| v.clone().to_data().as_str().unwrap_or("").to_string())
        .collect::<Vec<String>>();
    let re_pat = regex::Regex::new(&conf.anchor_text)?;

    for u in urls {
        if re_pat.is_match(&u) {
            return Ok(Some(Hit {
                version: version_str.to_string(),
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

        let resp = ureq::get(url)
                .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36")
                .call()?;
        let status_code = resp.status();

        debug!("Fetching {section}, status: {status_code}");
        match status_code {
            200..=299 => break resp,
            // https://developer.mozilla.org/en-US/docs/Web/HTTP/Status#client_error_responses
            408 | 425 | 429 | 500 | 502 | 503 | 504 => {
                let zzz = ((10 - attempts_remaining) * 4).min(60);
                info!("Got status {status_code} fetching {section}. Sleeping for {zzz} secs...");
                std::thread::sleep(Duration::from_secs(zzz));
                continue;
            }
            _ => {
                let body = resp.into_string()?;
                let msg = format!(
                    "Unexpected error fetching {url}. Status {status_code}. \
                    Body: {body}"
                );
                return Err(anyhow!(msg));
            }
        };
    };

    let body = resp.into_string()?;
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

fn extract_target_from_zipfile(compressed: &mut [u8], conf: &Config) -> Result<()> {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf)?;

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );

    let re_pat =
        make_re_target_filename(conf).expect("Failed to construct a regex for the target filename");

    for fname in archive
        .file_names()
        // What's dumb is that the borrow below `by_name` is a mutable
        // borrow, which means that an immutable borrow for
        // `archive.file_names` won't be allowed. To work around this,
        // for now just collect all the filenames into a long list.
        // Since we're looking for a specific name, it would be more
        // efficient to first find the name, leave the loop, and in the
        // next section do the extraction.
        .map(String::from)
        .collect::<Vec<String>>()
    {
        let mut file = archive.by_name(&fname)?;
        let path = Path::new(&fname);
        debug!(
            "zip, got filename: {}",
            &path.file_name().unwrap().to_str().unwrap()
        );
        if let Some(p) = &path.file_name() {
            if re_pat.is_match(p.to_str().unwrap()) {
                debug!("zip, Got a match: {}", &fname);
                let mut rawfile = std::fs::File::create(&target_filename)?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                rawfile.write_all(&buf)?;
                return Ok(());
            }
        }
    }

    warn!(
        "Failed to find file inside archive: \"{}\"",
        &target_filename
    );

    Ok(())
}

fn extract_target_from_gzfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = flate2::read::GzDecoder::new(&mut cbuf);

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );

    // If it's only `.gz` (and not `.tar.gz`) then it's a single file, so we don't
    // worry about trying to match a regex, just save whatever is there into the
    // `desired_filename`.

    let mut buf = vec![];
    archive.read_to_end(&mut buf).unwrap();
    let mut file = std::fs::File::create(target_filename).unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.write_all(&buf).unwrap();
}

fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) {
    // std::fs::write("compressed.tar.gz", &compressed).unwrap();

    let mut cbuf = std::io::Cursor::new(compressed);
    let gzip_archive = flate2::read::GzDecoder::new(&mut cbuf);
    let mut archive = tar::Archive::new(gzip_archive);

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );
    let re_pat =
        make_re_target_filename(conf).expect("Failed to construct a regex for the target filename");

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            if let Some(pm) = p.to_str() {
                if re_pat.is_match(pm) {
                    debug!("tar.gz, Got a match: {}", &pm);
                    file.unpack(&target_filename).unwrap();
                    return;
                }
            }
        }
    }

    warn!(
        "Failed to find file \"{}\" inside archive",
        &target_filename
    );
}

fn extract_target_from_tarxz(compressed: &mut [u8], conf: &Config) {
    let cbuf = std::io::Cursor::new(compressed);
    let mut decompressor = xz2::read::XzDecoder::new(cbuf);
    let mut archive = tar::Archive::new(&mut decompressor);

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );

    let re_pat =
        make_re_target_filename(conf).expect("Failed to construct a regex for the target filename");

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            if let Some(pm) = p.to_str() {
                if re_pat.is_match(pm) {
                    debug!("tar.gz, Got a match: {}", &pm);
                    file.unpack(&target_filename).unwrap();
                    return;
                }
            }
        }
    }

    warn!(
        "Failed to find file \"{}\" inside archive",
        &target_filename
    );
}

fn make_re_target_filename(conf: &Config) -> Result<regex::Regex> {
    let re = regex::Regex::new(
        format!(
            "^{}$",
            conf.target_filename_to_extract_from_archive
                .as_ref()
                .unwrap()
        )
        .as_str(),
    )?;
    Ok(re)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqrt() {
        assert!(true);
    }

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
        let conf = Config {
            template: "github_api_latest".to_string(),
            version: Some("13.0.0".to_string()),
            page_url: "https://api.github.com/repos/BurntSushi/ripgrep/releases/latest".to_string(),
            anchor_tag: "$.assets.*.browser_download_url".to_string(),
            anchor_text: r"ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz".to_string(),
            version_tag: Some("$.tag_name".to_string()),
            target_filename_to_extract_from_archive: Some("rg".to_string()),
            desired_filename: None,
        };
        let out = extract_data_from_json(payload, &conf)?;
        let expected_hit = Hit {
            version : "13.0.0".to_string(),
            download_url : "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-unknown-linux-musl.tar.gz".to_string()
        };
        assert_eq!(out, Some(expected_hit));
        Ok(())
    }
}
