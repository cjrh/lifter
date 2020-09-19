#[macro_use]
extern crate fstrings;

use anyhow::Result;
use itertools::Itertools;
use log::*;
use scraper::{Html, Selector};
use std::error::Error;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use structopt::StructOpt;

const PATTERN: &str = r###"(?P<binname>[a-zA-Z][a-zA-Z0-9_]+)-(?P<version>(?:[0-9]+\.[0-9]+)(?:\.[0-9]+)*)-(?P<platform>(?:[a-zA-Z0-9_]-?)+)"###;

#[derive(Default, Debug)]
struct Config {
    url_template: String,
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
            url_template: String::from("https://github.com/{project}/releases"),
            pattern: String::from(PATTERN),
            ..Default::default()
        }
    }
}

#[derive(structopt::StructOpt)]
#[structopt()]
struct Args {
    #[structopt(short = "p", long = "project", env = "PROJECT", default_value = "blah")]
    project: String,
    /// Silence all output
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,
    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: usize,
    /// Timestamp (sec, ms, ns, none)
    #[structopt(short = "t", long = "timestamp")]
    ts: Option<stderrlog::Timestamp>,
}

#[paw::main]
fn main(args: Args) -> Result<()> {
    stderrlog::new()
        .module(module_path!())
        .quiet(args.quiet)
        .verbosity(args.verbose)
        .timestamp(args.ts.unwrap_or(stderrlog::Timestamp::Off))
        .init()
        .unwrap();
    trace!("trace message");
    debug!("debug message");
    info!("info message");
    warn!("warn message");
    error!("error message");

    let filename = "binsync.config";
    let conf = tini::Ini::from_file(&filename).unwrap();
    for (section, hm) in conf.iter() {
        debug!("Checking {}...", &section);
        run_section(section, &conf, filename)?;
    }

    Ok(())
}

fn run_section(section: &str, conf: &tini::Ini, filename: &str) -> Result<()> {
    // Happy helper for getting a value in this section
    let get = |s: &str| conf.get::<String>(&section, s);
    let mut cf = Config::new();

    // First get the project - required
    match get("page_url") {
        Some(p) => cf.page_url = p,
        None => {
            return {
                warn!("Section {} is missing required field \"page_url\"", section);
                Ok(())
            };
        }
    };
    debug!("Processing: {}", &cf.page_url);

    // Now the remaining values
    cf.anchor_tag = get("anchor_tag").unwrap();
    cf.anchor_text = get("anchor_text").unwrap();
    cf.version_tag = get("version_tag");
    cf.target_filename_to_extract_from_archive =
        if let Some(name) = get("target_filename_to_extract_from_archive") {
            Some(name)
        } else {
            Some(section.to_owned())
        };
    cf.version = get("version");
    cf.desired_filename = get("desired_filename");

    if !std::path::Path::new(&cf.target_filename).exists() {
        if let Some(new_version) = process(&mut cf)? {
            // New version, must update the version number in the
            // config file.
            info!(
                "Downloaded new version of {}: {}",
                &cf.target_filename, &new_version
            );
            // TODO: actually need a mutex around the following 3 lines.
            let conf_write = tini::Ini::from_file(&filename).unwrap();

            conf_write
                .section(section)
                .item("version", &new_version)
                .to_file(&filename)
                .unwrap();
            debug!("Updated config file.");
        }
    } else {
        info!("Target {} exists, skipping.", &cf.target_filename);
    }
    Ok(())
}

fn process(conf: &mut Config) -> Result<Option<String>> {
    // TODO: can't use fstrings if we store the template. Instead,
    // we can use the string_template package.
    let url = &conf.page_url;
    debug!("Calling {}", &url);
    let resp = reqwest::blocking::get(url).unwrap();
    assert!(&resp.status().is_success());

    let body = resp.text()?;
    let fragment = Html::parse_document(&body);
    let stories = Selector::parse(&conf.anchor_tag).unwrap();
    let versions = Selector::parse(conf.version_tag.as_ref().unwrap()).unwrap();

    let re_pat = regex::Regex::new(&conf.anchor_text)?;

    for story in fragment.select(&stories) {
        if let Some(href) = &story.value().attr("href") {
            // debug!("Tag hit: {:?} {:?}", &story, &story.text());
            debug!("Found tag: {}", &href);

            let caps = match re_pat.captures_iter(&href).next() {
                Some(c) => c,
                None => continue,
            };

            // Version checking - must be done before we download files.
            let new_version: Option<String> = match fragment.select(&versions).next() {
                Some(m) => {
                    // debug!("{}", m.text().join(""));
                    info!("Found a match on versions tag: {:?}", m.text().join(""));
                    // if let Some(x) = m.value().attr("href") {
                    //     info!("Found a match on versions tag: {:?}", x);
                    // }
                    let existing_version = conf.version.as_ref().unwrap();
                    let found_version = m.text().join("");
                    if &found_version <= existing_version {
                        info!("Found version is not newer: {}; Skipping.", found_version);
                        return Ok(None);
                    } else {
                        info!(
                            "Found version {} is newer than existing version {}",
                            &found_version, existing_version
                        );
                        Some(found_version)
                    }
                }
                None => None,
            };

            // let new_version = match caps.name("version") {
            //     Some(version) => match &conf.version {
            //         Some(v) => {
            //             if v == version.as_str() {
            //                 Some(version.as_str().to_owned())
            //             } else {
            //                 None
            //             }
            //         }
            //         None => None,
            //     },
            //     None => None,
            // };

            let download_url = format!("https://github.com{}", &href);
            debug!("download_url: {}", &download_url);

            let mut resp = reqwest::blocking::get(&download_url).unwrap();
            let ext = {
                if vec![".tar.gz", ".tgz"]
                    .iter()
                    .any(|ext| href.ends_with(ext))
                {
                    ".tar.gz"
                } else if href.ends_with(".tar.xz") {
                    ".tar.xz"
                } else if href.ends_with(".zip") {
                    ".zip"
                } else if href.ends_with(".exe") {
                    ".exe"
                } else {
                    info!("Unknown file extension. Skipping.");
                    break;
                }
            };

            let mut buf: Vec<u8> = Vec::new();
            resp.copy_to(&mut buf)?;

            // if let Some(target_filename) = match conf.target_filename_to_extract_from_archive {
            //
            // };
            let dlfilename = if let Some(filename) = &conf.target_filename_to_extract_from_archive {
                filename
            } else if let Some(filename) = &conf.desired_filename {
                filename
            } else {
                return Err(anyhow::Error::msg(
                    "Either \"desired_filename\" or \"target_filename\" must be given",
                ));
            }
            .clone() + ext;

            if ext == ".tar.xz" {
                // This will handle both target filename extraction and renaming
                extract_target_from_tarxz(&mut buf, &conf);
                if let Some(desired_filename) = &conf.desired_filename {
                    let extracted_filename = conf
                        .target_filename_to_extract_from_archive
                        .as_ref()
                        .unwrap();
                    if desired_filename != extracted_filename {
                        debug!(
                            "Extract filename is different to desired, renaming {} \
                            to {}",
                            extracted_filename, desired_filename
                        );
                        std::fs::rename(extracted_filename, desired_filename)?;
                    }
                }
            } else if ext == ".zip" {
                // This will handle both target filename extraction and renaming
                extract_target_from_zipfile(&mut buf, &conf);
                if let Some(desired_filename) = &conf.desired_filename {
                    let extracted_filename = conf
                        .target_filename_to_extract_from_archive
                        .as_ref()
                        .unwrap();
                    if desired_filename != extracted_filename {
                        debug!(
                            "Extract filename is different to desired, renaming {} \
                            to {}",
                            extracted_filename, desired_filename
                        );
                        std::fs::rename(extracted_filename, desired_filename)?;
                    }
                }
            } else if ext == ".tar.gz" {
                // This will handle both target filename extraction and renaming
                extract_target_from_tarfile(&mut buf, &conf);
                if let Some(desired_filename) = &conf.desired_filename {
                    let extracted_filename = conf
                        .target_filename_to_extract_from_archive
                        .as_ref()
                        .unwrap();
                    if desired_filename != extracted_filename {
                        debug!(
                            "Extract filename is different to desired, renaming {} \
                            to {}",
                            extracted_filename, desired_filename
                        );
                        std::fs::rename(extracted_filename, desired_filename)?;
                    }
                }
            } else if ext == ".exe" {
                // Windows executables are not compressed, so we only need to
                // handle renames, if the option is given.
                let desired_filename = conf.desired_filename.as_ref().unwrap();
                let mut output = std::fs::File::create(&desired_filename)?;
                info!("Writing {} from {}", desired_filename, &href);
                output.write_all(&buf)?;
            };

            return Ok(new_version);
        }
    }

    Ok(None)
}

fn extract_target_from_zipfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf).unwrap();

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
        let mut file = archive.by_name(&fname).unwrap();
        let path = std::path::Path::new(&fname);
        debug!(
            "zip, got filename: {}",
            &path.file_name().unwrap().to_str().unwrap()
        );
        if let Some(p) = &path.file_name() {
            if &p.to_string_lossy() == target_filename {
                debug!("zip, Got a match: {}", &fname);
                let mut rawfile = std::fs::File::create(&target_filename).unwrap();
                let mut buf = Vec::new();
                file.read_to_end(&mut buf).unwrap();
                rawfile.write_all(&buf).unwrap();
                return;
            }
        }
    }

    warn!(
        "Failed to find file inside archive: \"{}\"",
        &target_filename
    );
}

fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut gzip_archive = flate2::read::GzDecoder::new(&mut cbuf);
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
        // println!("This is what I found in the tar: {:?}", &file.header());
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
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut buf: Vec<u8> = Vec::new();
    let mut bw = std::io::Cursor::new(&mut buf);

    // lzma_rs::xz_decompress(&mut cbuf, &mut bw).expect("Problem xz_decompress");

    let mut decompressor = xz2::read::XzDecoder::new(cbuf);

    // let mut xzf = lzma::LzmaReader::new_decompressor(cbuf).expect("Problem decompressing");

    // let decode_options = lzma_rs::decompress::Options {
    //     unpacked_size: lzma_rs::decompress::UnpackedSize::ReadFromHeader,
    // };
    // lzma_rs::lzma_decompress_with_options(&mut cbuf, &mut bw, &decode_options)
    //     .expect("Problem lzma_decompress_with_options");

    // let mut c = std::io::Cursor::new(&mut bw);
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
        debug!("This is what I found in the tar.xz: {:?}", &file.header());
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
