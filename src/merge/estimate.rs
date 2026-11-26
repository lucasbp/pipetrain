use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct ApiEstimate {
    pub discovery: u64,
    pub checks: u64,
    pub deploy_polls: u64,
    pub total: u64,
    pub budget: u32,
    pub exceeds_budget: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_remaining: Option<u64>,
    pub warn: bool,
}

impl ApiEstimate {
    pub fn warning(&self) -> Option<String> {
        if self.exceeds_budget {
            return Some(format!(
                "warning: about {} API requests, over the budget of {}; raise api.max_requests (this does not block)",
                self.total, self.budget
            ));
        }
        if self.warn {
            return Some(format!(
                "warning: about {} API requests, over half of the {} GitHub still allows (this does not block)",
                self.total,
                self.rate_remaining.unwrap_or_default()
            ));
        }
        None
    }
}

pub const POLL_FLAT_STEPS: u64 = 4;

pub fn poll_delay(base: Duration, ceiling: Duration, sleeps: u64) -> Duration {
    let ceiling = ceiling.max(base);
    if sleeps < POLL_FLAT_STEPS {
        return base;
    }
    let doublings = (sleeps - POLL_FLAT_STEPS + 1).min(31) as u32;
    base.saturating_mul(1u32 << doublings).min(ceiling)
}

pub fn estimated_polls(timeout: Duration, base: Duration, ceiling: Duration) -> u64 {
    let base = base.max(Duration::from_millis(1));
    let ceiling = ceiling.max(base);
    let mut elapsed = Duration::ZERO;
    let mut sleeps = 0u64;
    while elapsed < timeout {
        let delay = poll_delay(base, ceiling, sleeps);
        if delay >= ceiling {
            let rest = (timeout - elapsed).as_nanos().div_ceil(ceiling.as_nanos());
            sleeps = sleeps.saturating_add(u64::try_from(rest).unwrap_or(u64::MAX));
            break;
        }
        elapsed += delay;
        sleeps += 1;
    }
    sleeps.saturating_add(1)
}
