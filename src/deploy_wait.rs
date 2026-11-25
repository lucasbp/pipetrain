use std::time::{Duration, Instant};

use crate::github::Run;

pub const DEFAULT_QUEUED_TIMEOUT_MINUTES: u64 = 180;

#[derive(Debug, Clone)]
pub struct DeployClock {
    started: Instant,
    timeout: Duration,
    ceiling: Duration,
    parked: Duration,
    queued_since: Option<Instant>,
}

impl DeployClock {
    pub fn new(started: Instant, timeout: Duration, ceiling: Duration) -> Self {
        Self {
            started,
            timeout,
            ceiling,
            parked: Duration::ZERO,
            queued_since: None,
        }
    }

    pub fn observe(&mut self, now: Instant, queued: bool) -> Option<bool> {
        match (self.queued_since, queued) {
            (None, true) => {
                self.queued_since = Some(now);
                Some(true)
            }
            (Some(since), false) => {
                self.parked += now.saturating_duration_since(since);
                self.queued_since = None;
                Some(false)
            }
            _ => None,
        }
    }

    pub fn queued(&self) -> bool {
        self.queued_since.is_some()
    }

    pub fn queued_for(&self, now: Instant) -> Duration {
        self.parked
            + self
                .queued_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
            .saturating_sub(self.queued_for(now))
    }

    pub fn timed_out(&self, now: Instant) -> bool {
        self.elapsed(now) >= self.timeout
    }

    pub fn ceiling_hit(&self, now: Instant) -> bool {
        self.queued() && self.queued_for(now) >= self.ceiling
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        if self.queued() {
            self.ceiling.saturating_sub(self.queued_for(now))
        } else {
            self.timeout.saturating_sub(self.elapsed(now))
        }
    }

    pub fn ceiling(&self) -> Duration {
        self.ceiling
    }
}

pub fn waiting_for_runner(runs: &[&Run]) -> bool {
    runs.iter().any(|run| run.status == "queued")
        && !runs.iter().any(|run| run.status == "in_progress")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, seconds: u64) -> Instant {
        base + Duration::from_secs(seconds)
    }

    #[test]
    fn queued_time_does_not_count_toward_the_timeout() {
        let base = Instant::now();
        let mut clock = DeployClock::new(base, Duration::from_secs(60), Duration::from_secs(1000));
        assert_eq!(clock.observe(at(base, 10), true), Some(true));
        assert_eq!(clock.elapsed(at(base, 500)), Duration::from_secs(10));
        assert!(!clock.timed_out(at(base, 500)));
        assert_eq!(clock.observe(at(base, 500), false), Some(false));
        assert_eq!(clock.elapsed(at(base, 520)), Duration::from_secs(30));
        assert!(!clock.timed_out(at(base, 540)));
        assert!(clock.timed_out(at(base, 600)));
    }

    #[test]
    fn the_ceiling_only_applies_while_queued_and_accumulates() {
        let base = Instant::now();
        let mut clock = DeployClock::new(base, Duration::from_secs(60), Duration::from_secs(100));
        clock.observe(at(base, 0), true);
        assert!(!clock.ceiling_hit(at(base, 99)));
        assert!(clock.ceiling_hit(at(base, 100)));
        assert_eq!(clock.remaining(at(base, 40)), Duration::from_secs(60));
        clock.observe(at(base, 60), false);
        assert!(!clock.ceiling_hit(at(base, 1000)));
        clock.observe(at(base, 70), true);
        assert!(clock.ceiling_hit(at(base, 110)));
    }

    #[test]
    fn remaining_follows_the_timeout_when_not_queued() {
        let base = Instant::now();
        let clock = DeployClock::new(base, Duration::from_secs(60), Duration::from_secs(100));
        assert_eq!(clock.remaining(at(base, 15)), Duration::from_secs(45));
        assert_eq!(clock.remaining(at(base, 90)), Duration::ZERO);
    }

    fn run(status: &str) -> Run {
        Run {
            id: 1,
            workflow_id: 1,
            name: "deploy".to_string(),
            path: ".github/workflows/deploy.yml".to_string(),
            status: status.to_string(),
            conclusion: None,
            url: String::new(),
        }
    }

    #[test]
    fn only_a_queued_run_without_a_started_one_waits_for_a_runner() {
        let queued = run("queued");
        let going = run("in_progress");
        let done = run("completed");
        let held = run("waiting");
        assert!(waiting_for_runner(&[&queued]));
        assert!(waiting_for_runner(&[&queued, &done]));
        assert!(!waiting_for_runner(&[&queued, &going]));
        assert!(!waiting_for_runner(&[&going]));
        assert!(!waiting_for_runner(&[&held]));
        assert!(!waiting_for_runner(&[]));
    }
}
