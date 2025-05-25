mod btlog;
mod app;
use crate::btlog::log_error_with_stack_trace;
use anyhow::Result;
use itertools::Itertools;
use log::*;
use ratatui::prelude::*;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

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
    let (tx, rx): (
        Sender<lifter::event::ProgressEvent>,
        Receiver<lifter::event::ProgressEvent>,
    ) = mpsc::channel();
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
    std::env::set_current_dir(working_dir)?;

    let p = std::path::PathBuf::from(args.configfile);
    let filename = match p.exists() {
        true => p.to_string_lossy().to_string(),
        false => "lifter.ini".to_string(),
    };
    let conf = tini::Ini::from_file(&filename)?;
    // Let's iterate over the sections in conf, and convert them
    // into hashmaps. Each section will be a hashmap.
    let sections_raw: Vec<(String, HashMap<String, String>)> = conf
        .iter()
        .map(|(name, section)| {
            let mut inner_map = HashMap::new();
            section.iter().for_each(|(field, value)| {
                inner_map.insert(field.clone(), value.clone());
            });
            (name.to_string(), inner_map)
        })
        .collect();
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
                sections.push((name.clone(), section.clone()));
            };
        }
    });
    trace!("Detected templates: {:?}", templates);

    // Start the background worker thread. The purpose of this thread
    // is the have the blocking `.par_iter()` calls not block the UI
    // in the main thread, where we want to receive the events and render
    // the UI.
    let worker_handle = thread::spawn({
        // let tx = tx.clone();
        let templates = templates.clone();
        // let conf = conf.clone();
        // let conf = tini::Ini::from_file(&filename)?;
        move || {
            worker_loop(sections, &templates, &conf, &filename, tx);
        }
    });

    // Output
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut tui = ratatui::Terminal::new(backend)?;
    let mut app = app::App::default();

    // 4. main event loop
    'ui_loop: loop {
        // non-blocking receive to keep UI responsive
        for ev in rx.try_iter() {
            if matches!(ev, lifter::event::ProgressEvent::NoMoreWork) {
                break 'ui_loop;
            }
            app.handle_event(ev);
        }

        draw_ui(&mut tui, &app)?;
        thread::sleep(std::time::Duration::from_millis(17));
    }
    worker_handle.join().unwrap();

    Ok(())
}

fn worker_loop(
    sections: Vec<(String, HashMap<String, String>)>,
    templates: &HashMap<String, HashMap<String, String>>,
    conf: &tini::Ini,
    filename: &str,
    tx: Sender<lifter::event::ProgressEvent>,
) {
    // Let's make a mutex and pass it to each of the `run_section()` calls
    // that will run in separate threads. The mutex will be used to avoid
    // collisions when writing updates to the config file.
    use std::sync::Mutex;
    let mutex = Mutex::new(());

    sections.par_iter().for_each(|(section, _hm)| {
        let tx = tx.clone();
        match lifter::run_section(section, &templates, &conf, &filename, &mutex, tx) {
            Ok(_) => (),
            Err(e) => {
                log_error_with_stack_trace(format!("{}", e));
            }
        }
    });
}

fn draw_ui<B: ratatui::backend::Backend>(
    terminal: &mut ratatui::Terminal<B>,
    app: &app::App,
) -> anyhow::Result<()> {
    terminal.draw(|f| {
        use ratatui::{
            layout::{Constraint::*, Direction, Layout},
            widgets::*,
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Percentage(40), Percentage(60)].as_ref())
            .split(f.area());

        // 1. Active jobs
        let rows: Vec<Row> = app
            .active_jobs
            .iter()
            .map(|job| {
                let parts: Vec<&str> = job.splitn(2, ' ').collect();
                if parts.len() == 2 {
                    Row::new(vec![parts[0].to_string(), parts[1].to_string()])
                } else {
                    Row::new(vec![parts[0].to_string(), "".to_string()])
                }
            })
            .collect();

        let table =
            ratatui::widgets::Table::new(rows, [Constraint::Length(8), Constraint::Min(10)])
                .header(
                    Row::new(vec!["Worker", "Task"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .widths(&[Constraint::Length(8), Constraint::Min(10)]);

        f.render_widget(table, chunks[0]);

        // 2. Updated packages
        let items: Vec<ListItem> = app
            .updated
            .iter()
            .map(|p| ListItem::new(p.clone()))
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title("Updated packages")
                .borders(Borders::ALL),
        );

        f.render_widget(list, chunks[1]);
    })?;
    Ok(())
}

