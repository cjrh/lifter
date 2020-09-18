#[macro_use]
extern crate fstrings;

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
fn main(args: Args) -> Result<(), Box<dyn Error>> {
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
        // Happy helper for getting a value in this section
        let get = |s: &str| conf.get::<String>(&section, s);
        let mut cf = Config::new();

        // First get the project - required
        let project = match get("project") {
            Some(p) => p,
            None => continue,
        };
        debug!("Found the project section: {}", &project);
        cf.project = project.clone();

        // Now the remaining values
        cf.target_platform = get("target_platform");
        cf.version = get("version");
        if let Some(url_template) = get("url_template") {
            cf.url_template = url_template.clone();
            debug!("Set url_template: {:?}", &cf.url_template);
        }
        // println!("{:?}", &hm);
        if let Some(pattern) = get("pattern") {
            cf.pattern = pattern.clone();
            debug!("Set pattern: {:?}", &cf.pattern);
        };
        if let Some(tfn) = get("target_filename") {
            cf.target_filename = tfn.clone();
            debug!("Set target_filename: {:?}", &cf.target_filename);
        };
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
    }

    Ok(())
}

fn process(conf: &mut Config) -> Result<Option<String>, Box<dyn Error>> {
    // TODO: can't use fstrings if we store the template. Instead,
    // we can use the string_template package.
    let url = f!("https://github.com/{conf.project}/releases");
    let resp = reqwest::blocking::get(&url).unwrap();
    assert!(&resp.status().is_success());
    let body = resp.text()?;
    let fragment = Html::parse_document(&body);
    let stories = Selector::parse("details .Box a").unwrap();

    let re_pat = regex::Regex::new(&conf.pattern)?;

    for story in fragment.select(&stories) {
        if let Some(href) = &story.value().attr("href") {
            // println!("https://github.com{}", &href);

            let caps = match re_pat.captures_iter(&href).next() {
                Some(c) => c,
                None => continue,
            };

            if let Some(tp) = conf.target_platform.clone() {
                match caps.name("platform") {
                    Some(p) => {
                        // println!("Found platform in url: {}", &p.as_str());
                        if p.as_str() == tp {
                            debug!("Target platform {} detected", &p.as_str());
                        } else {
                            // println!("Platfom doesn't match");
                            continue;
                        }
                    }
                    None => {
                        warn!("No platform in the parsed URL");
                        continue;
                    }
                }
            };

            // println!("caps: {:?}", &caps);
            // if let Some(binname) = caps.name("binname") {
            //     println!("Got binname! {}", &binname.as_str());
            // }
            // if let Some(target_platform) = caps.name("platform") {
            //     println!("Got platform! {}", &target_platform.as_str());
            // }

            // Version checking - must be done before we download files.
            let new_version = match caps.name("version") {
                Some(version) => match &conf.version {
                    Some(v) => {
                        if v == version.as_str() {
                            Some(version.as_str().to_owned())
                        } else {
                            None
                        }
                    }
                    None => None,
                },
                None => None,
            };

            let download_url = format!("https://github.com{}", &href);
            debug!("{}", &download_url);

            let mut resp = reqwest::blocking::get(&download_url).unwrap();
            let ext = {
                if vec![".tar.gz", ".tgz"]
                    .iter()
                    .any(|ext| href.ends_with(ext))
                {
                    ".tar.gz"
                } else if href.ends_with(".zip") {
                    ".zip"
                } else if href.ends_with(".exe") {
                    ".exe"
                } else {
                    info!("Unknown file extension. Skipping.");
                    break;
                }
            };

            let dlfilename = conf.target_filename.clone() + ext;

            let mut buf: Vec<u8> = Vec::new();
            resp.copy_to(&mut buf)?;
            let mut cbuf = std::io::Cursor::new(&mut buf);

            if ext == ".zip" {
                extract_target_from_zipfile(&mut buf, &conf);
            } else if ext == ".tar.gz" {
                extract_target_from_tarfile(&mut buf, &conf);
            } else if ext == ".exe" {
            };

            let mut output = std::fs::File::create(&dlfilename)?;
            info!(
                "Writing {} from {}... output is {}",
                &conf.target_filename, &href, &dlfilename
            );
            output.write_all(&mut buf)?;
            return Ok(new_version);
        }
    }

    Ok(None)
}

fn extract_target_from_zipfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf).unwrap();

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
            if p.to_string_lossy() == conf.target_filename {
                debug!("zip, Got a match: {}", &fname);
                let mut rawfile = std::fs::File::create(&conf.target_filename).unwrap();
                let mut buf = Vec::new();
                file.read_to_end(&mut buf);
                rawfile.write_all(&buf);
            }
        }
    }
}

fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut gzip_archive = flate2::read::GzDecoder::new(&mut cbuf);
    let mut archive = tar::Archive::new(gzip_archive);
    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();

        // println!("This is what I found in the tar: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        if let Some(p) = &raw_path.file_name() {
            // println!("path: {:?}", &p);
            if let Some(pm) = p.to_str() {
                // println!("stem: {:?}", &pm);
                if pm == conf.target_filename {
                    // println!("We found a match: {}", &pm);
                    // println!("Raw headers: {:?}", &file.header());
                    file.unpack(&conf.target_filename).unwrap();
                    return;
                }
            }
        }
    }
}
