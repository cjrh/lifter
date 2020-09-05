#[macro_use]
extern crate fstrings;

#[macro_use]
extern crate ini;

use scraper::{Html, Selector};
use std::error::Error;
use std::io::{Read, Write};
const VERSION_EXTRACTOR: &str = r###"((?:[0-9]+\.[0-9]+)(?:\.[0-9]+)*)"###;
const TRIPLE_EXTRACTOR: &str = r###"-((?:[a-zA-Z0-9_-]+){3,4})"###;

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
struct Args {
    #[structopt(short = "p", long = "project", env = "PROJECT", default_value = "blah")]
    project: String,
}

#[paw::main]
fn main(args: Args) -> Result<(), Box<dyn Error>> {
    let conf = ini!("binsync.config");

    for (section, hm) in &conf {
        if let Some(project) = &hm["project"] {
            let mut cf = Config::new();
            cf.project = project.clone();
            cf.target_platform = hm["target_platform"].clone();
            cf.version = hm["version"].clone();
            if let Some(Some(url_template)) = hm.get("url_template") {
                cf.url_template = url_template.clone();
            }
            if let Some(Some(pattern)) = hm.get("pattern") {
                cf.pattern = pattern.clone();
            };
            if let Some(Some(tfn)) = hm.get("target_filename") {
                cf.target_filename = tfn.clone();
            };
            process(&mut cf)?;
        }
    }

    return Ok(());
}

fn process(conf: &mut Config) -> Result<(), Box<dyn Error>> {
    // TODO: can't use fstrings if we store the template. Instead,
    // we can use the string_template package.
    let url = f!("https://github.com/{conf.project}/releases");
    let resp = reqwest::blocking::get(&url).unwrap();
    assert!(&resp.status().is_success());
    let body = resp.text()?;
    let fragment = Html::parse_document(&body);
    let stories = Selector::parse("details .Box a").unwrap();

    let re = regex::Regex::new(&VERSION_EXTRACTOR)?;
    let re_triple = regex::Regex::new(&TRIPLE_EXTRACTOR)?;
    let re_pat = regex::Regex::new(&PATTERN)?;

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
                            println!("Target platform {} detected", &p.as_str());
                        } else {
                            // println!("Platfom doesn't match");
                            continue;
                        }
                    }
                    None => {
                        println!("No platform in the parsed URL");
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
            // if let Some(version) = caps.name("version") {
            //     println!("Got version! {}", &version.as_str());
            // }
            let download_url = format!("https://github.com{}", &href);
            println!("{}", &download_url);

            let mut resp = reqwest::blocking::get(&download_url).unwrap();
            // let ext = std::path::Path::new(&href)
            //     .extension()
            //     .and_then(std::ffi::OsStr::to_str)
            //     .unwrap();
            let ext = {
                if href.ends_with(".tar.gz") {
                    ".tar.gz"
                } else if href.ends_with(".tgz") {
                    ".tar.gz"
                } else if href.ends_with(".zip") {
                    ".zip"
                } else {
                    println!("Unknown file extension. Skipping.");
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
            };

            let mut output = std::fs::File::create(&dlfilename)?;
            println!("Writing {} from {}...", &conf.target_filename, &href);
            output.write_all(&mut buf);
            // resp.copy_to(&mut output)?;

            break;
        }
    }

    Ok(())
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
        println!(
            "zip, got filename: {}",
            &path.file_name().unwrap().to_str().unwrap()
        );
        if let Some(p) = &path.file_name() {
            if p.to_string_lossy() == conf.target_filename {
                println!("zip, Got a match: {}", &fname);
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
