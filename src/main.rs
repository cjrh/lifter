use anyhow::Result;
use itertools::Itertools;
use lifter::add::AddGithubOptions;
use lifter::RunContext;
use log::*;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const LONG_ABOUT: &str = "\
Download single-file binaries from GitHub Releases (and other sites) listed in
a lifter.config file, skipping downloads when the cached version is already
up to date.

Diagnostic logs are written to stderr; one CSV row per config section is
written to stdout, with columns:

    timestamp,updated,tool_name,file_name,previous_version,current_version

Because stdout is pure CSV, lifter composes cleanly with awk / grep / etc.

EXAMPLES:
    # Run with the default config file (./lifter.config).
    lifter

    # Show only tools that were updated this run (updated == 1). Logs go to
    # stderr, so the pipe to awk sees only CSV rows.
    lifter | awk -F, '$2==1'

    # Append a changelog entry for every updated tool.
    lifter | awk -F, '$2==1 { printf(\"%s  %s: %s -> %s\\n\", $1, $3, $5, $6) }' \\
        >> CHANGELOG

    # Crank verbosity but keep the pipe clean by sending logs to a file.
    lifter -vv 2>lifter.log | awk -F, '$2==1'

    # Run 8 downloads in parallel, restricted to a couple of sections.
    lifter -vv -x 8 -f ripgrep,fzf

    # Append a GitHub Releases definition to the active config.
    lifter add github BurntSushi/ripgrep --extract rg

    # A GitHub token raises the API rate limit, and is effectively required
    # for configs with many github_api_latest entries.
    GITHUB_TOKEN=ghp_xxxxxxxxxxxx lifter -vv
";

#[derive(structopt::StructOpt)]
#[structopt(
    about = "Download single-file binaries from GitHub Releases and similar sites.",
    long_about = LONG_ABOUT,
)]
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
    // TODO: should use XDG_CONFIG style locations for config
    /// Output directory. By default, the same directory
    /// that the lifter binary is in.
    #[structopt(parse(from_os_str), short = "w", long = "working-dir")]
    working_dir: Option<std::path::PathBuf>,
    /// Config file with the download definitions. When omitted, lifter
    /// looks for lifter.config (then lifter.ini) in the current
    /// directory and then alongside the lifter executable.
    #[structopt(short = "c", long = "config-file")]
    configfile: Option<String>,
    /// Only run these names. Comma separated.
    #[structopt(short = "f", long = "filter")]
    filter: Option<String>,
    /// Number of parallel download workers
    #[structopt(short = "x", long = "threads", default_value = "1")]
    threads: usize,
    #[structopt(subcommand)]
    command: Option<Command>,
}

#[derive(structopt::StructOpt)]
enum Command {
    /// Add a new download definition to the active config
    Add(AddArgs),
}

#[derive(structopt::StructOpt)]
struct AddArgs {
    #[structopt(subcommand)]
    command: AddCommand,
}

#[derive(structopt::StructOpt)]
enum AddCommand {
    /// Add a GitHub Releases definition using the latest release metadata
    Github(AddGithubArgs),
}

#[derive(structopt::StructOpt)]
struct AddGithubArgs {
    /// GitHub repository in OWNER/REPO form
    repo: String,
    /// Config section name. Defaults to the repository name.
    #[structopt(long = "name")]
    section_name: Option<String>,
    /// Substring used to choose a release asset when inference is ambiguous
    #[structopt(long = "asset")]
    asset_filter: Option<String>,
    /// Archive member to extract, e.g. --extract rg
    #[structopt(long = "extract")]
    extract: Option<String>,
    /// Print the generated entry without modifying the config
    #[structopt(long = "dry-run")]
    dry_run: bool,
}

/// Find the configuration file lifter reads from and writes to.
///
/// When `requested` is `Some`, it is an explicit `--config-file` value
/// and is honoured verbatim (resolved against the current directory like
/// any other relative path). When it is `None`, lifter searches so it
/// can be run from any directory: the current directory first, then the
/// directory containing the lifter executable, trying `lifter.config`
/// and then the legacy `lifter.ini` in each. The executable's directory
/// is the important case — it is where a typical install keeps the
/// config alongside the binaries it manages, so `lifter` and
/// `lifter add` update that one config no matter where they are invoked
/// from. The returned path is always absolute.
///
/// If no config exists yet, the path in the executable's directory is
/// returned so `add` creates it where future runs will look.
fn resolve_config_path(requested: Option<&str>) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    if let Some(path) = requested {
        let p = PathBuf::from(path);
        return Ok(if p.is_absolute() { p } else { cwd.join(p) });
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));

    const NAMES: [&str; 2] = ["lifter.config", "lifter.ini"];
    for dir in [Some(cwd.clone()), exe_dir.clone()].iter().flatten() {
        for name in NAMES {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // No config exists yet: create it next to the executable so future
    // runs (from any directory) find it, falling back to the current
    // directory only when the executable's location is unknown.
    Ok(exe_dir.unwrap_or(cwd).join("lifter.config"))
}

fn run_command(command: Command, config_path: &std::path::Path) -> Result<()> {
    match command {
        Command::Add(add_args) => match add_args.command {
            AddCommand::Github(github_args) => {
                let added = lifter::add::add_github_definition(
                    config_path,
                    &AddGithubOptions {
                        repo: github_args.repo,
                        section_name: github_args.section_name,
                        asset_filter: github_args.asset_filter,
                        extract: github_args.extract,
                        dry_run: github_args.dry_run,
                    },
                )?;
                if added.wrote_file {
                    eprintln!(
                        "Added [{}] to {} using asset {}",
                        added.section_name,
                        added.config_path.display(),
                        added.asset_name
                    );
                    if added.wrote_template {
                        eprintln!("Also added missing [template:github_api_latest] template");
                    }
                } else {
                    eprintln!(
                        "Dry run: generated [{}] from asset {}",
                        added.section_name, added.asset_name
                    );
                }
                print!("{}", added.entry);
            }
        },
    }
    Ok(())
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

    // Resolve the config first (relative to the real CWD), then derive
    // the working directory from it so downloads land next to the
    // config. This is what lets `lifter` (and `lifter add`) be invoked
    // from any directory yet still update the one config that lives
    // alongside the managed binaries.
    let config_path = resolve_config_path(args.configfile.as_deref())?;
    let working_dir = args.working_dir.unwrap_or_else(|| {
        config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("."))
    });
    // The downloads no longer rely on CWD — they're written to
    // `working_dir` explicitly via `run_section`, and the config path is
    // already absolute — but keep the CWD aligned with `working_dir` for
    // any incidental relative-path behaviour.
    std::env::set_current_dir(&working_dir)?;

    let filename = config_path.to_string_lossy().to_string();

    if let Some(command) = args.command {
        return run_command(command, &config_path);
    }

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

    // Shared per-run state: one mutex guarding INI writes, one
    // serializing CSV rows on stdout. `run_section` emits its own CSV
    // row per section (including on error) and logs errors to stderr,
    // so the caller has nothing to do with the return value.
    let ctx = RunContext::new();

    sections.par_iter().for_each(|(section, _hm)| {
        lifter::run_section(section, &templates, &conf, &filename, &working_dir, &ctx);
    });

    Ok(())
}
