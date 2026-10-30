use std::collections::{BTreeMap, VecDeque};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::config::ApiConfig;

pub(super) struct Guard {
    max_total: u32,
    max_per_minute: u32,
    min_remaining: u64,
    pub(super) total: u32,
    stamps: VecDeque<Instant>,
    pub(super) remaining: BTreeMap<String, u64>,
    verbose: bool,
}

impl Guard {
    pub(super) fn new(cfg: &ApiConfig, verbose: bool) -> Self {
        Self {
            max_total: cfg.max_requests,
            max_per_minute: cfg.max_per_minute.max(1),
            min_remaining: cfg.min_remaining,
            total: 0,
            stamps: VecDeque::new(),
            remaining: BTreeMap::new(),
            verbose,
        }
    }

    pub(super) fn before(&mut self, resource: &str, label: &str) -> Result<()> {
        if self.total >= self.max_total {
            bail!(
                "stopped after {} API requests (api.max_requests); raise it in the config if this is expected",
                self.total
            );
        }
        if let Some(&left) = self.remaining.get(resource)
            && left < self.min_remaining
        {
            bail!(
                "stopped: only {left} GitHub {resource} API requests left in this window (api.min_remaining is {})",
                self.min_remaining
            );
        }
        let window = Duration::from_secs(60);
        while self.stamps.front().is_some_and(|t| t.elapsed() >= window) {
            self.stamps.pop_front();
        }
        if self.stamps.len() >= self.max_per_minute as usize {
            if let Some(oldest) = self.stamps.front() {
                let wait = window.saturating_sub(oldest.elapsed());
                if self.verbose {
                    eprintln!("pacing: waiting {}s", wait.as_secs() + 1);
                }
                sleep(wait + Duration::from_millis(250));
            }
            self.stamps.pop_front();
        }
        self.stamps.push_back(Instant::now());
        self.total += 1;
        if self.verbose {
            eprintln!("request {:>3}  {label}", self.total);
        }
        Ok(())
    }
}
