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
    #[structopt(parse(from_os_str), short = "o", long = "output-dir")]
    output_dir: Option<std::path::PathBuf>,
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

    let filename = "lifter.config";
    let conf = tini::Ini::from_file(&filename)?;
    let sections_raw = conf.iter().collect_vec();
    let current_dir = std::env::current_dir()?;
    let output_dir = args.output_dir.or(Some(current_dir)).unwrap();

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
    sections_raw.into_iter()
        .for_each(|(name, seciter)| {
            if name.starts_with("template:") {
                // This inner map (inside a particular template)
                // will store each of the fields and values
                // for that template.
                let mut inner_map = HashMap::new();
                seciter.for_each(|(field, value)| {
                    inner_map.insert(field.clone(), value.clone());
                });

                templates.insert(
                    name.strip_prefix("template:").unwrap().to_string(),
                    inner_map,
                );
            } else {
                // This is not a template so move it into 
                // the "real" sections list.
                sections.push((name.clone(), seciter));
            }
        });
    trace!("Detected templates: {:?}", templates);

    sections.par_iter().for_each(|(section, _hm)| {
        match lifter::run_section(section, &conf, filename, &output_dir) {
            Ok(_) => (),
            Err(e) => {
                error!("{}", e);
            }
        }
    });

    Ok(())
}
