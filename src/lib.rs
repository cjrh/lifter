use std::io::{Read, Write};
#[cfg(target_family = "unix")]
use std::os::unix::fs::PermissionsExt;

use anyhow::{anyhow, Result};
use itertools::Itertools;
use log::*;
use scraper::{Html, Selector};
use std::collections::HashMap;
use std::path::Path;
use strfmt::strfmt;

/// This pattern matches the format of how filenames of binaries are
/// usually written out on github. It will match things like:
///
/// lifter-0.13.4-linux-x86_64
///
/// The groups will pull out these fields:
/// binname: "lifter"
/// version: "0.13.4"
/// platform: "linux-x86_64"

#[derive(Default, Debug)]
struct Config {
    template: String,
    project: String,
    pattern: String,
    version: Option<String>,
    target_platform: Option<String>,
    target_filename: String,

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
            // TODO: the latest should be the default here.
            template: String::from("https://github.com/{project}/releases"),
            ..Default::default()
        }
    }
}

struct Hit {
    version: String,
    download_url: String,
}

fn read_section_into_map(conf: &tini::Ini, section: &str) -> HashMap<String, String> {
    let mut tmp = HashMap::new();
    conf.section_iter(&section).for_each(|(k, v)| {
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
    };

    debug!("Substitutions complete: {:?}", &cf);
    Ok(())
}

pub fn run_section(
    section: &str,
    templates: &Templates,
    conf: &tini::Ini,
    filename: &str,
    output_dir: &Path,
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
            Some(strfmt(value, &tmp)?)
        } else {
            Some(section.to_owned())
        };

    if let Some(value) = tmp.get("version") {
        cf.version = Some(strfmt(value, &tmp)?);
    };

    cf.desired_filename = if let Some(value) = tmp.get("desired_filename") {
        Some(strfmt(value, &tmp)?)
    } else {
        cf.target_filename_to_extract_from_archive.clone()
    };

    if !Path::new(&cf.target_filename).exists() {
        if let Some(new_version) = process(section, &mut cf, output_dir)? {
            // New version, must update the version number in the
            // config file.
            info!(
                "[{}] Downloaded new version of {}: {}",
                section, &cf.target_filename, &new_version
            );
            // TODO: actually need a mutex around the following 3 lines.
            let conf_write = tini::Ini::from_file(&filename).unwrap();
            conf_write
                .section(section)
                .item("version", &new_version)
                .to_file(&filename)
                .unwrap();
            debug!("[{}] Updated config file.", section);
        }
    } else {
        info!(
            "[{}] Target {} exists, skipping.",
            section, &cf.target_filename
        );
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

fn process(section: &str, conf: &mut Config, output_dir: &Path) -> Result<Option<String>> {
    let url = &conf.page_url;

    let parse_result = parse_html_page(section, &conf, url)?;
    let hit = match parse_result {
        Some(hit) => hit,
        None => return Ok(None),
    };

    let existing_version = conf.version.as_ref().unwrap();
    if target_file_already_exists(&conf) && &hit.version <= existing_version {
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
        } else if download_url.ends_with(".tar.xz") {
            ".tar.xz"
        } else if download_url.ends_with(".zip") {
            ".zip"
        } else if download_url.ends_with(".exe") {
            ".exe"
        } else if let Some(suffix) = slice_from_end(&download_url, 8) {
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

    let mut resp = reqwest::blocking::get(download_url)?;
    let mut buf: Vec<u8> = Vec::new();
    resp.copy_to(&mut buf)?;

    if ext == ".tar.xz" {
        extract_target_from_tarxz(&mut buf, &conf);
    } else if ext == ".zip" {
        extract_target_from_zipfile(&mut buf, &conf)?;
    } else if ext == ".tar.gz" {
        extract_target_from_tarfile(&mut buf, &conf);
    } else if vec![".exe", ""].contains(&ext) {
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

    if let Some(desired_filename) = &conf.desired_filename {
        let extracted_filename = conf
            .target_filename_to_extract_from_archive
            .as_ref()
            .unwrap();
        if desired_filename != extracted_filename {
            debug!(
                "[{}] Extract filename is different to desired, renaming {} \
                 to {}",
                section, extracted_filename, desired_filename
            );
            // TODO: this must be updated to handle output_dir
            std::fs::rename(extracted_filename, desired_filename)?;
        }
    }

    if let Some(filename) = &conf.desired_filename {
        if ext != ".exe" {
            // TODO: this must be updated to handle output_dir
            set_executable(&filename)?;
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

fn parse_html_page(section: &str, conf: &Config, url: &str) -> Result<Option<Hit>> {
    debug!("[{}] Fetching page at {}", section, &url);
    let resp = reqwest::blocking::get(url)?;
    let body = resp.text()?;

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
    let re_pat = regex::Regex::new(&conf.anchor_text)?;

    debug!("[{}] Looking for matches...", section);
    for story in fragment.select(&stories) {
        if let Some(href) = &story.value().attr("href") {
            // This is the download target in the matched link
            let download_url = format!("https://github.com{}", &href);
            debug!("[{}] download_url: {}", section, &download_url);

            if !re_pat.is_match(&href) {
                continue;
            }
            debug!("[{}] Found a match for anchor_text", section);

            return if let Some(raw_version) = fragment.select(&versions).next() {
                let version = raw_version.text().join("");
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

    let target_filename = conf
        .target_filename_to_extract_from_archive
        .as_ref()
        .expect(
            "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
        );

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
            if &p.to_string_lossy() == target_filename {
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

fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let gzip_archive = flate2::read::GzDecoder::new(&mut cbuf);
    let mut archive = tar::Archive::new(gzip_archive);

    let target_filename = conf
        .target_filename_to_extract_from_archive
        .as_ref()
        .expect(
            "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
        );

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            // println!("path: {:?}", &p);
            if let Some(pm) = p.to_str() {
                // println!("stem: {:?}", &pm);
                if pm == target_filename {
                    debug!("tar.gz, Got a match: {}", &pm);
                    // println!("We found a match: {}", &pm);
                    // println!("Raw headers: {:?}", &file.header());
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

    let target_filename = conf
        .target_filename_to_extract_from_archive
        .as_ref()
        .expect(
            "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
        );

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            // println!("path: {:?}", &p);
            if let Some(pm) = p.to_str() {
                // println!("stem: {:?}", &pm);
                if pm == target_filename {
                    debug!("tar.gz, Got a match: {}", &pm);
                    // println!("We found a match: {}", &pm);
                    // println!("Raw headers: {:?}", &file.header());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqrt() {
        assert!(true);
    }
}
