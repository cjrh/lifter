use anyhow::Result;
use itertools::Itertools;
use log::*;
use rayon::prelude::*;
use std::collections::HashMap;

#[derive(structopt::StructOpt)]
#[structopt()]
struct Args {
    /// Silence all output
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,
    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: usize,
    /// Timestamp (sec, ms, ns, none)
    #[structopt(short = "t", long = "timestamp")]
    ts: Option<stderrlog::Timestamp>,
    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(parse(from_os_str), short = "w", long = "working-dir")]
    // TODO: should use XDG_CONFIG style locations for config
    /// Output directory. By default, the same directory
    /// that the lifter binary is in.
    working_dir: Option<std::path::PathBuf>,
    /// The config file to use for the download definitions
    #[structopt(short = "c", long = "config-file", default_value = "lifter.config")]
    configfile: String,
    /// Only run these names. Comma separated.
    #[structopt(short = "f", long = "filter")]
    filter: Option<String>,
}

#[paw::main]
fn main(args: Args) -> Result<()> {
    // We're using threads for IO, so we can use more than cpu count
    rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build_global()
        .unwrap();

    stderrlog::new()
        .module(module_path!())
        .module("ureq")
        .quiet(args.quiet)
        .verbosity(args.verbose)
        .timestamp(args.ts.unwrap_or(stderrlog::Timestamp::Off))
        .init()
        .unwrap();

    let current_dir = std::env::current_dir()?;
    let working_dir = args.working_dir.unwrap_or(current_dir);
    std::env::set_current_dir(working_dir)?;

    let p = std::path::PathBuf::from(args.configfile);
    let filename = match p.exists() {
        true => p.to_string_lossy().to_string(),
        false => "lifter.ini".to_string(),
    };
    let conf = tini::Ini::from_file(&filename)?;
    let sections_raw = conf.iter().collect_vec();
    let filters = args.filter.or_else(|| Some("".to_string())).unwrap();
    let filters = filters.split(',').map(|s| s.trim()).collect::<Vec<_>>();

    // One of the sections in the .ini file could be a group of
    // templates. A template is a collection of fields with
    // default values. A "real" (non-template) section can
    // refer to a template by name. When this happens, the
    // fields in that template will get substituted into
    // that section's values.
    //
    // Before we do anything, collect all the template sections
    // and separate them out from the "real" sections

    // This will hold the templates. The key is the name
    // of the template and the value is another hashmap of
    // each of the fields and field values within that template.
    let mut templates = HashMap::new();
    // This will hold the "real" sections
    let mut sections = vec![];
    sections_raw.into_iter().for_each(|(name, section)| {
        if name.starts_with("template:") {
            // This inner map (inside a particular template)
            // will store each of the fields and values
            // for that template.
            debug!("Processing template: {}", name);
            let mut inner_map = HashMap::new();
            section.iter().for_each(|(field, value)| {
                inner_map.insert(field.clone(), value.clone());
            });

            templates.insert(
                name.strip_prefix("template:").unwrap().to_string(),
                inner_map,
            );
        } else {
            // This is not a template so move it into
            // the "real" sections list; but, only if it is not
            // being filtered out.
            let included = filters.is_empty() || filters.iter().any(|f| name.contains(f));
            if included {
                debug!("Processing section: {}", name);
                sections.push((name.clone(), section));
            };
        }
    });
    trace!("Detected templates: {:?}", templates);

    sections.par_iter().for_each(|(section, _hm)| {
        match lifter::run_section(section, &templates, &conf, &filename) {
            Ok(_) => (),
            Err(e) => {
                error!("{}", e);
            }
        }
    });

    Ok(())
}
