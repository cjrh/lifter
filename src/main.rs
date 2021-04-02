use anyhow::Result;
use itertools::Itertools;
use log::*;
use rayon::prelude::*;

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
    let conf = tini::Ini::from_file(&filename).unwrap();
    let sections = conf.iter().collect_vec();
    let current_dir = std::env::current_dir()?;
    let output_dir = args.output_dir.or(Some(current_dir)).unwrap();
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
