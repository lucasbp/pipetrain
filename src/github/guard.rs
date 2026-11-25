use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::error::ApiError;
use crate::config::{ApiConfig, Reserve};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub core: u32,
    pub graphql: u32,
    pub search: u32,
    pub cache_hits: u32,
    pub not_modified: u32,
}

impl Usage {
    pub fn spent(&self) -> u32 {
        self.core + self.graphql + self.search
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Below {
    pub resource: String,
    pub left: u64,
    pub floor: u64,
    pub reset: Option<i64>,
}

#[derive(Clone)]
pub(super) struct Guard {
    shared: Arc<Mutex<Budget>>,
    verbose: bool,
}

struct Budget {
    max_total: u32,
    max_per_minute: u32,
    min_remaining: u64,
    reserve: Reserve,
    per_second: u32,
    tokens: f64,
    refilled: Instant,
    total: u32,
    stamps: VecDeque<Instant>,
    remaining: BTreeMap<String, (u64, Option<i64>)>,
    limits: BTreeMap<String, u64>,
    usage: Usage,
}

impl Guard {
    pub(super) fn new(cfg: &ApiConfig, verbose: bool) -> Self {
        let per_second = cfg.max_per_second.max(1);
        Self {
            shared: Arc::new(Mutex::new(Budget {
                max_total: cfg.max_requests,
                max_per_minute: cfg.max_per_minute.max(1),
                min_remaining: cfg.min_remaining,
                reserve: cfg.reserve,
                per_second,
                tokens: f64::from(per_second),
                refilled: Instant::now(),
                total: 0,
                stamps: VecDeque::new(),
                remaining: BTreeMap::new(),
                limits: BTreeMap::new(),
                usage: Usage::default(),
            })),
            verbose,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Budget> {
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn total(&self) -> u32 {
        self.lock().total
    }

    pub(super) fn usage(&self) -> Usage {
        self.lock().usage
    }

    pub(super) fn reserve(&self) -> Reserve {
        self.lock().reserve
    }

    pub(super) fn remaining(&self) -> Option<u64> {
        self.lock().remaining.values().map(|(left, _)| *left).min()
    }

    pub(super) fn observe(
        &self,
        resource: &str,
        left: u64,
        reset: Option<i64>,
        limit: Option<u64>,
    ) {
        let mut budget = self.lock();
        if let Some(limit) = limit {
            budget.limits.insert(resource.to_string(), limit);
        }
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

    pub(super) fn below_reserve(&self) -> Option<Below> {
        let budget = self.lock();
        budget
            .remaining
            .iter()
            .filter_map(|(resource, &(left, reset))| {
                let limit = *budget.limits.get(resource)?;
                let floor = budget.reserve.floor(limit);
                (left < floor).then(|| Below {
                    resource: resource.clone(),
                    left,
                    floor,
                    reset,
                })
            })
            .min_by_key(|below| below.left)
    }

    pub(super) fn hit(&self) {
        let mut budget = self.lock();
        budget.usage.cache_hits = budget.usage.cache_hits.saturating_add(1);
        drop(budget);
        crate::log::stats::cache_hit();
    }

    pub(super) fn refund(&self, resource: &str) {
        let mut budget = self.lock();
        budget.total = budget.total.saturating_sub(1);
        let slot = budget.usage.slot(resource);
        *slot = slot.saturating_sub(1);
        budget.usage.not_modified = budget.usage.not_modified.saturating_add(1);
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
                budget.refill();
                if budget.stamps.len() >= budget.max_per_minute as usize {
                    budget.stamps.front().map(|oldest| {
                        window.saturating_sub(oldest.elapsed()) + Duration::from_millis(250)
                    })
                } else if budget.tokens < 1.0 {
                    let missing = 1.0 - budget.tokens;
                    Some(Duration::from_secs_f64(
                        missing / f64::from(budget.per_second),
                    ))
                } else {
                    budget.tokens -= 1.0;
                    budget.stamps.push_back(Instant::now());
                    budget.total += 1;
                    *budget.usage.slot(resource) += 1;
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
                if self.verbose && wait >= Duration::from_secs(1) {
                    crate::progress::line(&format!("pacing: waiting {}s", wait.as_secs() + 1));
                }
                sleep(wait);
            }
        }
    }
}

impl Usage {
    fn slot(&mut self, resource: &str) -> &mut u32 {
        match resource {
            "graphql" => &mut self.graphql,
            "search" | "code_search" => &mut self.search,
            _ => &mut self.core,
        }
    }
}

impl Budget {
    fn refill(&mut self) {
        let now = Instant::now();
        let gained = now.duration_since(self.refilled).as_secs_f64() * f64::from(self.per_second);
        self.tokens = (self.tokens + gained).min(f64::from(self.per_second));
        self.refilled = now;
    }

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
                max_per_second: 1000,
                ..ApiConfig::default()
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
        other.observe("core", 9, None, None);
        assert_eq!(guard.remaining(), Some(9));
        assert!(guard.before("core", "test").is_err());
        other.observe("core", 20, None, None);
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
        guard.observe("core", 9, Some(100), None);
        guard.observe("core", 1000, Some(200), None);
        guard.observe("core", 1, Some(100), None);
        assert_eq!(guard.remaining(), Some(1000));
        guard.before("core", "test").unwrap();
    }

    #[test]
    fn default_reserve_is_twenty_percent_of_the_limit() {
        let guard = guard(50);
        assert_eq!(guard.reserve().floor(5000), 1000);
        guard.observe("core", 1001, Some(10), Some(5000));
        assert_eq!(guard.below_reserve(), None);
        guard.observe("core", 999, Some(10), Some(5000));
        let below = guard.below_reserve().unwrap();
        assert_eq!((below.left, below.floor), (999, 1000));
    }

    #[test]
    fn reserve_is_per_resource() {
        let guard = guard(50);
        guard.observe("core", 4000, Some(10), Some(5000));
        guard.observe("graphql", 100, Some(10), Some(5000));
        assert_eq!(guard.below_reserve().unwrap().resource, "graphql");
    }

    #[test]
    fn refunded_requests_are_not_spent() {
        let guard = guard(50);
        guard.before("core", "a").unwrap();
        guard.before("graphql", "b").unwrap();
        guard.refund("core");
        assert_eq!(guard.total(), 1);
        let usage = guard.usage();
        assert_eq!((usage.core, usage.graphql, usage.not_modified), (0, 1, 1));
    }

    #[test]
    fn the_burst_is_limited_by_requests_per_second() {
        let guard = Guard::new(
            &ApiConfig {
                max_per_second: 4,
                max_per_minute: 1000,
                max_requests: 100,
                min_remaining: 0,
                ..ApiConfig::default()
            },
            false,
        );
        let started = Instant::now();
        for _ in 0..4 {
            guard.before("core", "burst").unwrap();
        }
        assert!(started.elapsed() < Duration::from_millis(150));
        for _ in 0..4 {
            guard.before("core", "paced").unwrap();
        }
        assert!(started.elapsed() >= Duration::from_millis(850));
        assert_eq!(guard.total(), 8);
    }
}
