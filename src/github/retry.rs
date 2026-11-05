use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use super::reply::Reply;

const LONGEST_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub max_retries: u32,
    pub base: Duration,
    pub cap: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Server(u16),
    SecondaryLimit,
}

impl Cause {
    pub fn describe(self) -> String {
        match self {
            Self::Server(status) => format!("answered {status}"),
            Self::SecondaryLimit => "hit its secondary rate limit".to_string(),
        }
    }
}

fn mentions_secondary_limit(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("secondary rate limit") || lower.contains("abuse detection")
}

impl Retry {
    pub const fn standard() -> Self {
        Self {
            max_retries: 4,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(20),
        }
    }

    pub const fn off() -> Self {
        Self {
            max_retries: 0,
            base: Duration::ZERO,
            cap: Duration::ZERO,
        }
    }

    pub fn cause(reply: &Reply, retry_after: Option<Duration>) -> Option<Cause> {
        match reply.status {
            500 | 502 | 503 | 504 => Some(Cause::Server(reply.status)),
            403 | 429 if reply.rate.remaining != Some(0) => {
                let secondary = reply.status == 429
                    || retry_after.is_some()
                    || mentions_secondary_limit(&reply.message());
                secondary.then_some(Cause::SecondaryLimit)
            }
            _ => None,
        }
    }

    pub fn wait(&self, retried: u32, retry_after: Option<Duration>) -> Option<Duration> {
        if retried >= self.max_retries {
            return None;
        }
        if let Some(asked) = retry_after {
            return (asked <= LONGEST_RETRY_AFTER).then(|| asked + jitter(self.base, retried));
        }
        let doubled = self
            .base
            .saturating_mul(2u32.saturating_pow(retried))
            .min(self.cap);
        let half = doubled / 2;
        Some(half + jitter(half, retried))
    }
}

fn jitter(max: Duration, salt: u32) -> Duration {
    if max.is_zero() {
        return Duration::ZERO;
    }
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(salt);
    let millis = u64::try_from(max.as_millis()).unwrap_or(u64::MAX).max(1);
    Duration::from_millis(hasher.finish() % (millis + 1))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::github::reply::Rate;

    fn reply(status: u16, remaining: Option<u64>, body: serde_json::Value) -> Reply {
        Reply {
            status,
            rate: Rate {
                limit: None,
                remaining,
                reset: None,
                resource: "core".to_string(),
            },
            body,
        }
    }

    #[test]
    fn server_errors_are_retried() {
        for status in [500, 502, 503, 504] {
            let found = Retry::cause(&reply(status, None, json!({})), None);
            assert_eq!(found, Some(Cause::Server(status)));
        }
        assert_eq!(Retry::cause(&reply(501, None, json!({})), None), None);
        assert_eq!(Retry::cause(&reply(404, None, json!({})), None), None);
        assert_eq!(Retry::cause(&reply(200, None, json!({})), None), None);
    }

    #[test]
    fn secondary_limits_are_retried_but_the_primary_one_is_not() {
        let secondary = json!({"message": "You have exceeded a secondary rate limit."});
        assert_eq!(
            Retry::cause(&reply(403, Some(4000), secondary.clone()), None),
            Some(Cause::SecondaryLimit)
        );
        assert_eq!(
            Retry::cause(&reply(403, Some(0), secondary), None),
            None,
            "an exhausted quota resets in up to an hour, so it is reported, not waited out"
        );
        let plain = json!({"message": "Resource not accessible"});
        assert_eq!(
            Retry::cause(&reply(403, Some(4000), plain.clone()), None),
            None
        );
        assert_eq!(
            Retry::cause(&reply(403, Some(4000), plain), Some(Duration::from_secs(5))),
            Some(Cause::SecondaryLimit)
        );
        assert_eq!(
            Retry::cause(&reply(429, Some(4000), json!({})), None),
            Some(Cause::SecondaryLimit)
        );
    }

    #[test]
    fn the_wait_grows_stays_under_the_cap_and_stops_after_the_last_retry() {
        let policy = Retry::standard();
        let mut ceilings = Vec::new();
        for retried in 0..policy.max_retries {
            let waited = policy.wait(retried, None).expect("a wait");
            let ceiling = policy
                .base
                .saturating_mul(2u32.pow(retried))
                .min(policy.cap);
            assert!(waited >= ceiling / 2 && waited <= ceiling, "{waited:?}");
            ceilings.push(ceiling);
        }
        assert!(ceilings.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(policy.wait(policy.max_retries, None), None);
    }

    #[test]
    fn retry_after_is_honored_unless_it_is_too_long() {
        let policy = Retry::standard();
        let short = policy.wait(0, Some(Duration::from_secs(7))).unwrap();
        assert!(short >= Duration::from_secs(7));
        assert_eq!(policy.wait(0, Some(Duration::from_secs(600))), None);
    }

    #[test]
    fn off_never_waits() {
        assert_eq!(Retry::off().wait(0, None), None);
    }
}
