use anyhow::Result;
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use rand::distributions::{Distribution, Uniform};
use ratatui::{prelude::*, widgets::*};

use std::sync::mpsc::{Receiver, Sender};

const NUM_DOWNLOADS: usize = 10;

type DownloadId = usize;
type WorkerId = usize;

enum Event {
    Input(crossterm::event::KeyEvent),
    Tick,
    Resize,
    DownloadUpdate(WorkerId, DownloadId, f64),
    DownloadDone(WorkerId, DownloadId),
}

struct Downloads {
    pending: VecDeque<Download>,
    in_progress: BTreeMap<WorkerId, DownloadInProgress>,
}

impl Downloads {
    fn next(&mut self, worker_id: WorkerId) -> Option<Download> {
        match self.pending.pop_front() {
            Some(d) => {
                self.in_progress.insert(
                    worker_id,
                    DownloadInProgress {
                        id: d.id,
                        started_at: Instant::now(),
                        progress: 0.0,
                    },
                );
                Some(d)
            }
            None => None,
        }
    }
}

struct DownloadInProgress {
    id: DownloadId,
    started_at: Instant,
    progress: f64,
}

struct Download {
    id: DownloadId,
    size: usize,
}

struct Worker {
    id: WorkerId,
    tx: mpsc::Sender<Download>,
}

pub fn ui_main(backend_rx: Receiver<crate::events::LifterEvent>) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(8),
        },
    )?;

    let (tx, rx) = mpsc::channel();
    input_handling(tx.clone());
    let workers = workers(tx);
    let mut downloads = downloads();

    for w in &workers {
        let d = downloads.next(w.id).unwrap();
        w.tx.send(d).unwrap();
    }

    run_app(&mut terminal, workers, downloads, rx, backend_rx)?;

    crossterm::terminal::disable_raw_mode()?;
    terminal.clear()?;

    Ok(())
}

fn input_handling(tx: mpsc::Sender<Event>) {
    let tick_rate = Duration::from_millis(200);
    thread::spawn(move || {
        let mut last_tick = Instant::now();
        loop {
            // poll for tick rate duration, if no events, sent tick event.
            let timeout = tick_rate.saturating_sub(last_tick.elapsed());
            if crossterm::event::poll(timeout).unwrap() {
                match crossterm::event::read().unwrap() {
                    crossterm::event::Event::Key(key) => tx.send(Event::Input(key)).unwrap(),
                    crossterm::event::Event::Resize(_, _) => tx.send(Event::Resize).unwrap(),
                    _ => {}
                };
            }
            if last_tick.elapsed() >= tick_rate {
                tx.send(Event::Tick).unwrap();
                last_tick = Instant::now();
            }
        }
    });
}

fn workers(tx: mpsc::Sender<Event>) -> Vec<Worker> {
    (0..4)
        .map(|id| {
            let (worker_tx, worker_rx) = mpsc::channel::<Download>();
            let tx = tx.clone();
            thread::spawn(move || {
                while let Ok(download) = worker_rx.recv() {
                    let mut remaining = download.size;
                    while remaining > 0 {
                        let wait = (remaining as u64).min(10);
                        thread::sleep(Duration::from_millis(wait * 10));
                        remaining = remaining.saturating_sub(10);
                        let progress = (download.size - remaining) * 100 / download.size;
                        tx.send(Event::DownloadUpdate(id, download.id, progress as f64))
                            .unwrap();
                    }
                    tx.send(Event::DownloadDone(id, download.id)).unwrap();
                }
            });
            Worker { id, tx: worker_tx }
        })
        .collect()
}

fn downloads() -> Downloads {
    let distribution = Uniform::new(0, 1000);
    let mut rng = rand::thread_rng();
    let pending = (0..NUM_DOWNLOADS)
        .map(|id| {
            let size = distribution.sample(&mut rng);
            Download { id, size }
        })
        .collect();
    Downloads {
        pending,
        in_progress: BTreeMap::new(),
    }
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    workers: Vec<Worker>,
    mut downloads: Downloads,
    rx: mpsc::Receiver<Event>,
    rx_backend: Receiver<crate::events::LifterEvent>,
) -> Result<()> {
    let mut redraw = true;
    loop {
        if redraw {
            terminal.draw(|f| ui(f, &downloads))?;
        }
        redraw = true;

        use crate::events::LifterEvent;

        // match rx_backend.try_recv() {
        //     Ok(crate::events::LifterEvent::Hello(i)) => {
        //         terminal.insert_before(1, |buf| {
        //             Paragraph::new(format!("Hello {}", i)).render(buf.area, buf);
        //         })?;
        //     }
        //     Ok(crate::events::LifterEvent::Message(s)) => {
        //         terminal.insert_before(1, |buf| {
        //             Paragraph::new(s).render(buf.area, buf);
        //         })?;
        //     }
        //     Err(mpsc::TryRecvError::Empty) => {}
        //     Err(mpsc::TryRecvError::Disconnected) => break,
        // }

        match rx_backend.recv()? {
            LifterEvent::Input(event) => {
                if event.code == crossterm::event::KeyCode::Char('q') {
                    break;
                }
            }
            LifterEvent::Resize => {
                terminal.autoresize()?;
            }
            LifterEvent::Tick => {}
            LifterEvent::Hello(i) => {
                terminal.insert_before(1, |buf| {
                    Paragraph::new(format!("Hello {}", i)).render(buf.area, buf);
                })?;
            }
            LifterEvent::Message(s) => {
                terminal.insert_before(1, |buf| {
                    Paragraph::new(s).render(buf.area, buf);
                })?;

                downloads
                    .in_progress
                    .entry(0)
                    .or_insert(DownloadInProgress {
                        id: 0,
                        started_at: Instant::now(),
                        progress: 50.0,
                    });
            } // LifterEvent::DownloadUpdate(worker_id, _download_id, progress) => {
              //     let download = downloads.in_progress.get_mut(&worker_id).unwrap();
              //     download.progress = progress;
              //     redraw = false
              // }
              // LifterEvent::DownloadDone(worker_id, download_id) => {
              //     let download = downloads.in_progress.remove(&worker_id).unwrap();
              //     terminal.insert_before(1, |buf| {
              //         Paragraph::new(Line::from(vec![
              //             Span::from("Finished "),
              //             Span::styled(
              //                 format!("download {download_id}"),
              //                 Style::default().add_modifier(Modifier::BOLD),
              //             ),
              //             Span::from(format!(
              //                 " in {}ms",
              //                 download.started_at.elapsed().as_millis()
              //             )),
              //         ]))
              //         .render(buf.area, buf);
              //     })?;
              //     match downloads.next(worker_id) {
              //         Some(d) => workers[worker_id].tx.send(d).unwrap(),
              //         None => {
              //             if downloads.in_progress.is_empty() {
              //                 terminal.insert_before(1, |buf| {
              //                     Paragraph::new("Done !").render(buf.area, buf);
              //                 })?;
              //                 break;
              //             }
              //         }
              //     };
              // }
        };
    }
    Ok(())
}

fn ui(f: &mut Frame, downloads: &Downloads) {
    let area = f.size();

    let block = Block::default().title(block::Title::from("Progress").alignment(Alignment::Center));
    f.render_widget(block, area);

    let vertical = Layout::vertical([Constraint::Length(2), Constraint::Length(4)]).margin(1);
    let horizontal = Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(80)]);
    let [progress_area, main] = vertical.areas(area);
    let [list_area, gauge_area] = horizontal.areas(main);

    // total progress
    let done = NUM_DOWNLOADS - downloads.pending.len() - downloads.in_progress.len();
    let progress = LineGauge::default()
        .gauge_style(Style::default().fg(Color::Blue))
        .label(format!("{done}/{NUM_DOWNLOADS}"))
        .ratio(done as f64 / NUM_DOWNLOADS as f64);
    f.render_widget(progress, progress_area);

    // in progress downloads
    let items: Vec<ListItem> = downloads
        .in_progress
        .values()
        .map(|download| {
            ListItem::new(Line::from(vec![
                Span::raw(symbols::DOT),
                Span::styled(
                    format!(" download {:>2}", download.id),
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    " ({}ms)",
                    download.started_at.elapsed().as_millis()
                )),
            ]))
        })
        .collect();
    let list = List::new(items);
    f.render_widget(list, list_area);

    for (i, (_, download)) in downloads.in_progress.iter().enumerate() {
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Yellow))
            .ratio(download.progress / 100.0);
        if gauge_area.top().saturating_add(i as u16) > area.bottom() {
            continue;
        }
        f.render_widget(
            gauge,
            Rect {
                x: gauge_area.left(),
                y: gauge_area.top().saturating_add(i as u16),
                width: gauge_area.width,
                height: 1,
            },
        );
    }
}
