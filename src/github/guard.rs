use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::error::ApiError;
use crate::config::ApiConfig;

#[derive(Clone)]
pub(super) struct Guard {
    shared: Arc<Mutex<Budget>>,
    verbose: bool,
}

struct Budget {
    max_total: u32,
    max_per_minute: u32,
    min_remaining: u64,
    total: u32,
    stamps: VecDeque<Instant>,
    remaining: BTreeMap<String, (u64, Option<i64>)>,
}

impl Guard {
    pub(super) fn new(cfg: &ApiConfig, verbose: bool) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Budget {
                max_total: cfg.max_requests,
                max_per_minute: cfg.max_per_minute.max(1),
                min_remaining: cfg.min_remaining,
                total: 0,
                stamps: VecDeque::new(),
                remaining: BTreeMap::new(),
            })),
            verbose,
        }
    }

    pub(super) fn total(&self) -> u32 {
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .total
    }

    pub(super) fn remaining(&self) -> Option<u64> {
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remaining
            .values()
            .map(|(left, _)| *left)
            .min()
    }

    pub(super) fn observe(&self, resource: &str, left: u64, reset: Option<i64>) {
        let mut budget = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = budget
            .remaining
            .entry(resource.to_string())
            .or_insert((left, reset));
        if reset > entry.1 {
            *entry = (left, reset);
        } else if reset == entry.1 {
            entry.0 = entry.0.min(left);
        }
    }
    pub(super) fn before(&self, resource: &str, label: &str) -> Result<(), ApiError> {
        loop {
            let wait = {
                let mut budget = self.shared.lock().map_err(|_| ApiError::Stopped {
                    message: "the shared GitHub request budget is unavailable".to_string(),
                })?;
                budget.check_limits(resource)?;
                let window = Duration::from_secs(60);
                while budget.stamps.front().is_some_and(|t| t.elapsed() >= window) {
                    budget.stamps.pop_front();
                }
                if budget.stamps.len() >= budget.max_per_minute as usize {
                    budget.stamps.front().map(|oldest| {
                        window.saturating_sub(oldest.elapsed()) + Duration::from_millis(250)
                    })
                } else {
                    budget.stamps.push_back(Instant::now());
                    budget.total += 1;
                    let total = budget.total;
                    drop(budget);
                    crate::log::stats::request();
                    if self.verbose {
                        crate::progress::line(&format!("request {total:>3}  {label}"));
                    }
                    return Ok(());
                }
            };
            if let Some(wait) = wait {
                if self.verbose {
                    crate::progress::line(&format!("pacing: waiting {}s", wait.as_secs() + 1));
                }
                sleep(wait);
            }
        }
    }
}

impl Budget {
    fn check_limits(&self, resource: &str) -> Result<(), ApiError> {
        if self.total >= self.max_total {
            return Err(ApiError::Stopped {
                message: format!(
                    "stopped after {} API requests (api.max_requests); raise it in the config if this is expected",
                    self.total
                ),
            });
        }
        if let Some(&(left, _)) = self.remaining.get(resource)
            && left < self.min_remaining
        {
            return Err(ApiError::Stopped {
                message: format!(
                    "stopped: only {left} GitHub {resource} API requests left in this window (api.min_remaining is {})",
                    self.min_remaining
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn guard(max_total: u32) -> Guard {
        Guard::new(
            &ApiConfig {
                max_requests: max_total,
                max_per_minute: 1000,
                min_remaining: 10,
            },
            false,
        )
    }

    #[test]
    fn clones_share_request_budget_atomically() {
        let guard = guard(7);
        let accepted = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..32 {
                let guard = guard.clone();
                let accepted = &accepted;
                scope.spawn(move || {
                    if guard.before("core", "test").is_ok() {
                        accepted.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(accepted.load(Ordering::SeqCst), 7);
        assert_eq!(guard.total(), 7);
    }

    #[test]
    fn clones_share_rate_limit_observations() {
        let guard = guard(50);
        let other = guard.clone();
        other.observe("core", 9, None);
        assert_eq!(guard.remaining(), Some(9));
        assert!(guard.before("core", "test").is_err());
        other.observe("core", 20, None);
        assert_eq!(guard.remaining(), Some(9));
    }

    #[test]
    fn pacing_releases_budget_lock_while_sleeping() {
        let guard = guard(50);
        {
            let mut budget = guard.shared.lock().unwrap();
            budget.max_per_minute = 1;
            budget
                .stamps
                .push_back(Instant::now() - Duration::from_millis(59_950));
        }
        let other = guard.clone();
        let worker = std::thread::spawn(move || other.before("core", "test"));
        std::thread::sleep(Duration::from_millis(10));
        assert!(guard.shared.try_lock().is_ok());
        worker.join().unwrap().unwrap();
        assert_eq!(guard.total(), 1);
    }
    #[test]
    fn newer_rate_window_replaces_old_and_late_old_responses_do_not_restore_it() {
        let guard = guard(50);
        guard.observe("core", 9, Some(100));
        guard.observe("core", 1000, Some(200));
        guard.observe("core", 1, Some(100));
        assert_eq!(guard.remaining(), Some(1000));
        guard.before("core", "test").unwrap();
    }
}
