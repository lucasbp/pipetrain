use std::io::Stderr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::apply::{Report, Running, Status, action_count, flat_index, live_rows, statuses};
use crate::live::{Live, Row};
use crate::progress::{self, mark_cursor_hidden, terminal_width};

const REDRAW: Duration = Duration::from_millis(80);
const DEFAULT_WIDTH: usize = 80;

struct Shared {
    live: Live<Stderr>,
    rows: Vec<Row>,
    frame: usize,
    running: bool,
    stop: bool,
}

impl Shared {
    fn redraw(&mut self) {
        self.live.draw(&self.rows, self.frame);
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) struct Checklist {
    shared: Arc<Mutex<Shared>>,
    starts: Vec<Option<Instant>>,
    took: Vec<Option<Duration>>,
    handle: Option<JoinHandle<()>>,
}

impl Checklist {
    pub(super) fn start(report: &Report) -> Option<Checklist> {
        if !progress::prepare_terminal() {
            return None;
        }
        let count = action_count(report);
        let width = terminal_width().unwrap_or(DEFAULT_WIDTH);
        let mut live = Live::new(std::io::stderr(), progress::color_enabled(), width);
        let rows = live_rows(report, None, &[]);
        live.draw(&rows, 0);
        mark_cursor_hidden(true);
        let shared = Arc::new(Mutex::new(Shared {
            live,
            rows,
            frame: 0,
            running: false,
            stop: false,
        }));
        let worker = Arc::clone(&shared);
        let handle = thread::spawn(move || {
            loop {
                thread::park_timeout(REDRAW);
                let mut state = lock(&worker);
                if state.stop {
                    return;
                }
                if state.running {
                    state.frame += 1;
                    state.redraw();
                }
            }
        });
        Some(Checklist {
            shared,
            starts: vec![None; count],
            took: vec![None; count],
            handle: Some(handle),
        })
    }

    pub(super) fn update(&mut self, report: &Report, running: Running) {
        match running {
            Some(position) => self.starts[flat_index(report, position)] = Some(Instant::now()),
            None => self.settle(report),
        }
        let mut state = lock(&self.shared);
        state.rows = live_rows(report, running, &self.took);
        state.running = running.is_some();
        state.redraw();
    }

    fn settle(&mut self, report: &Report) {
        for (index, status) in statuses(report).into_iter().enumerate() {
            if status == Status::Planned || self.took[index].is_some() {
                continue;
            }
            if let Some(start) = self.starts[index] {
                self.took[index] = Some(start.elapsed());
            }
        }
    }

    pub(super) fn finish(mut self) {
        self.close();
    }

    fn close(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        {
            let mut state = lock(&self.shared);
            state.stop = true;
            state.running = false;
            state.redraw();
            state.live.finish();
        }
        mark_cursor_hidden(false);
        handle.thread().unpark();
        let _ = handle.join();
    }
}

impl Drop for Checklist {
    fn drop(&mut self) {
        self.close();
    }
}
