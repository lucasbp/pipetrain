use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone, Utc};

use crate::config::{ApiConfig, Reserve};
use crate::github::{Below, Client, Window};

pub const EXIT_QUOTA: i32 = 6;

const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
const PLAN_CORE_BASE: u64 = 30;
const PLAN_CORE_PER_TARGET: u64 = 3;
const PLAN_GRAPHQL_PER_ORG: u64 = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Need {
    pub core: u64,
    pub graphql: u64,
    pub search: u64,
}

impl Need {
    fn of(&self, resource: &str) -> u64 {
        match resource {
            "core" => self.core,
            "graphql" => self.graphql,
            "search" => self.search,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    pub resource: String,
    pub need: u64,
    pub left: u64,
    pub floor: u64,
    pub missing: u64,
    pub reset: Option<i64>,
}

#[derive(Debug)]
pub struct QuotaRefused(pub String);

impl fmt::Display for QuotaRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for QuotaRefused {}

pub fn plan_need(orgs: usize, targets: usize) -> Need {
    Need {
        core: PLAN_CORE_BASE + PLAN_CORE_PER_TARGET * targets as u64,
        graphql: PLAN_GRAPHQL_PER_ORG * orgs.max(1) as u64,
        search: 0,
    }
}

pub fn shortfalls(
    windows: &BTreeMap<String, Window>,
    reserve: Reserve,
    need: &Need,
) -> Vec<Shortfall> {
    ["core", "graphql", "search"]
        .into_iter()
        .filter_map(|resource| {
            let window = windows.get(resource)?;
            let wanted = need.of(resource);
            let floor = reserve.floor(window.limit);
            let spare = window.remaining.saturating_sub(floor);
            (wanted > spare).then(|| Shortfall {
                resource: resource.to_string(),
                need: wanted,
                left: window.remaining,
                floor,
                missing: wanted + floor - window.remaining.min(wanted + floor),
                reset: window.reset,
            })
        })
        .collect()
}

pub fn clock(reset: i64) -> String {
    let Some(utc) = DateTime::<Utc>::from_timestamp(reset, 0) else {
        return "an unknown time".to_string();
    };
    let local = Local.from_utc_datetime(&utc.naive_utc());
    format!(
        "{} UTC ({} local)",
        utc.format("%Y-%m-%d %H:%M"),
        local.format("%H:%M")
    )
}

pub fn message(shortfalls: &[Shortfall], reserve: Reserve, now: i64) -> String {
    let mut text = format!(
        "not enough GitHub API quota to start (api.reserve is {reserve}), nothing was changed:\n"
    );
    for gap in shortfalls {
        text.push_str(&format!(
            "  {}: needs about {}, has {} left with a reserve of {}, missing {}\n",
            gap.resource, gap.need, gap.left, gap.floor, gap.missing
        ));
    }
    match shortfalls.iter().filter_map(|gap| gap.reset).max() {
        Some(reset) => text.push_str(&format!(
            "The quota resets at {}, {}. Run the command again then, or lower api.reserve.",
            clock(reset),
            crate::github::describe_reset(now, Some(reset))
        )),
        None => text.push_str("GitHub did not say when the quota resets."),
    }
    text
}

pub fn stop_message(below: &Below) -> String {
    let when = below
        .reset
        .map_or_else(|| "at an unknown time".to_string(), clock);
    format!(
        "stopped before merging: only {} GitHub {} API requests left, below the reserve of {} (api.reserve); no new merge starts. The quota resets at {when}; run the command again then to resume",
        below.left, below.resource, below.floor
    )
}

pub fn backoff(delay: Duration, ceiling: Duration, below: &Below, step: u64) -> Duration {
    let base = ceiling.max(Duration::from_secs(1)) * 2;
    let slow = (base * 2u32.pow(step.min(6) as u32)).min(MAX_BACKOFF);
    let slow = slow.max(delay);
    match below.reset {
        Some(reset) => {
            let until = u64::try_from(reset - Utc::now().timestamp()).unwrap_or(0);
            if until == 0 {
                delay
            } else {
                slow.min(Duration::from_secs(until + 2))
            }
        }
        None => slow,
    }
}

pub fn refuse(client: &mut Client, cfg: &ApiConfig, need: &Need) -> Result<(), QuotaRefused> {
    let Some(windows) = client.quota_probe() else {
        return Ok(());
    };
    let gaps = shortfalls(&windows, cfg.reserve, need);
    if gaps.is_empty() {
        return Ok(());
    }
    Err(QuotaRefused(message(
        &gaps,
        cfg.reserve,
        Utc::now().timestamp(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(core_left: u64) -> BTreeMap<String, Window> {
        BTreeMap::from([(
            "core".to_string(),
            Window {
                limit: 5000,
                remaining: core_left,
                reset: Some(1_800_000_000),
            },
        )])
    }

    #[test]
    fn the_reserve_is_subtracted_before_comparing() {
        let need = Need {
            core: 300,
            ..Need::default()
        };
        let reserve = Reserve::Percent(20.0);
        assert!(shortfalls(&windows(1300), reserve, &need).is_empty());
        let gaps = shortfalls(&windows(1200), reserve, &need);
        assert_eq!(gaps.len(), 1);
        assert_eq!((gaps[0].floor, gaps[0].missing), (1000, 100));
    }

    #[test]
    fn an_absolute_reserve_above_a_small_limit_does_not_refuse_an_unused_resource() {
        let mut all = windows(4900);
        all.insert(
            "search".to_string(),
            Window {
                limit: 30,
                remaining: 30,
                reset: Some(1_800_000_000),
            },
        );
        assert!(shortfalls(&all, Reserve::Absolute(200), &Need::default()).is_empty());
    }

    #[test]
    fn the_message_names_the_gap_and_both_clocks() {
        let need = Need {
            core: 300,
            ..Need::default()
        };
        let gaps = shortfalls(&windows(900), Reserve::Percent(20.0), &need);
        let text = message(&gaps, Reserve::Percent(20.0), 1_799_999_000);
        assert!(
            text.contains("core: needs about 300, has 900 left"),
            "{text}"
        );
        assert!(text.contains("missing 400"), "{text}");
        assert!(text.contains("UTC ("), "{text}");
        assert!(text.contains("local)"), "{text}");
    }
}
