use anyhow::Result;
use itertools::Itertools;
use log::*;
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::stdout;

use ratatui::{
    prelude::{CrosstermBackend, Stylize, Terminal},
    widgets::Paragraph,
};

use crossterm::{
    event::{self, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};

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
    #[structopt(short = "x", long = "threads", default_value = "1")]
    threads: usize,
}

#[paw::main]
fn main(args: Args) -> Result<()> {
    // We're using threads for IO, so we can use more than cpu count
    rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
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
    std::env::set_current_dir(&working_dir)?;

    let filters = args.filter.or_else(|| Some("".to_string())).unwrap();
    let filters = filters.split(',').map(|s| s.trim()).collect::<Vec<_>>();

    // Use the new scoped threads feature
    use lifter::events::LifterEvent;
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel::<LifterEvent>();

        scope.spawn(|| -> Result<()> {
            lifter::process_templates(&working_dir, &args.configfile, &filters, tx)
        });

        info!("Starting UI");
        lifter::ui::ui_main(rx).unwrap();
    });
    Ok(())
}
