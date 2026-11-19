use std::time::Duration;

use pipetrain::config::MergeConfig;
use pipetrain::merge::{ApiEstimate, POLL_FLAT_STEPS, estimated_polls, poll_delay};

fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn sequence(base: u64, ceiling: u64, count: u64) -> Vec<u64> {
    (0..count)
        .map(|index| poll_delay(secs(base), secs(ceiling), index).as_secs())
        .collect()
}

fn simulated_polls(timeout: Duration, base: Duration, ceiling: Duration) -> u64 {
    let mut elapsed = Duration::ZERO;
    let mut sleeps = 0;
    while elapsed < timeout {
        let remaining = timeout - elapsed;
        elapsed += poll_delay(base, ceiling, sleeps).min(remaining);
        sleeps += 1;
    }
    sleeps + 1
}

#[test]
fn the_first_four_sleeps_use_the_base_interval_then_the_interval_doubles_to_the_ceiling() {
    assert_eq!(POLL_FLAT_STEPS, 4);
    assert_eq!(
        sequence(15, 60, 9),
        vec![15, 15, 15, 15, 30, 60, 60, 60, 60]
    );
    assert_eq!(sequence(5, 60, 9), vec![5, 5, 5, 5, 10, 20, 40, 60, 60]);
}

#[test]
fn a_ceiling_below_the_base_disables_the_backoff() {
    assert_eq!(sequence(15, 0, 8), vec![15; 8]);
    assert_eq!(sequence(15, 10, 8), vec![15; 8]);
    assert_eq!(sequence(15, 15, 8), vec![15; 8]);
}

#[test]
fn the_default_ceiling_is_sixty_seconds() {
    assert_eq!(MergeConfig::default().poll_max_seconds, 60);
    assert_eq!(MergeConfig::default().poll_seconds, 15);
}

#[test]
fn the_backoff_never_stretches_the_total_deadline() {
    for timeout in [1u64, 59, 60, 61, 90, 1800, 3600] {
        let timeout = secs(timeout);
        let mut elapsed = Duration::ZERO;
        let mut sleeps = 0;
        while elapsed < timeout {
            let remaining = timeout - elapsed;
            elapsed += poll_delay(secs(15), secs(60), sleeps).min(remaining);
            sleeps += 1;
        }
        assert_eq!(elapsed, timeout);
    }
}

#[test]
fn the_estimate_simulates_the_scale_and_is_smaller_than_the_fixed_interval_count() {
    let timeout = secs(30 * 60);
    let fixed = timeout.as_secs().div_ceil(15) + 1;
    let scaled = estimated_polls(timeout, secs(15), secs(60));
    assert_eq!(fixed, 121);
    assert_eq!(scaled, 35);
    assert!(scaled < fixed);
}

#[test]
fn the_estimate_matches_the_simulated_loop_and_never_undercounts_it() {
    for timeout in [0u64, 1, 14, 15, 61, 90, 91, 600, 1800, 7200] {
        for (base, ceiling) in [(15u64, 60u64), (1, 4), (5, 60), (15, 15), (15, 0)] {
            let timeout = secs(timeout);
            let estimate = estimated_polls(timeout, secs(base), secs(ceiling));
            assert_eq!(
                estimate,
                simulated_polls(timeout, secs(base), secs(ceiling)),
                "timeout {timeout:?} base {base} ceiling {ceiling}"
            );
        }
    }
}

#[test]
fn without_a_ceiling_above_the_base_the_estimate_is_the_old_fixed_count() {
    assert_eq!(estimated_polls(secs(1800), secs(15), secs(0)), 121);
    assert_eq!(estimated_polls(secs(31), secs(15), secs(60)), 4);
}

fn estimate(total: u64, budget: u32, rate_remaining: Option<u64>) -> ApiEstimate {
    let exceeds_budget = total > u64::from(budget);
    ApiEstimate {
        discovery: 0,
        checks: 0,
        deploy_polls: total,
        total,
        budget,
        exceeds_budget,
        rate_remaining,
        warn: exceeds_budget || rate_remaining.is_some_and(|left| total > left / 2),
    }
}

#[test]
fn the_warning_only_fires_over_the_budget_or_over_half_of_the_known_rate_limit() {
    assert!(estimate(200, 300, None).warning().is_none());
    assert!(estimate(200, 300, Some(5000)).warning().is_none());
    assert!(estimate(300, 300, Some(5000)).warning().is_none());
    assert!(estimate(301, 300, None).warning().is_some());
    assert!(estimate(200, 300, Some(399)).warning().is_some());
    assert!(estimate(200, 300, Some(400)).warning().is_none());
}

#[test]
fn the_warning_is_one_short_line_for_each_trigger() {
    let budget = estimate(658, 300, None).warning().unwrap();
    assert_eq!(budget.lines().count(), 1);
    assert!(budget.contains("658"));
    assert!(budget.contains("raise api.max_requests"));
    assert!(budget.len() < 120, "{budget}");
    let rate = estimate(200, 300, Some(300)).warning().unwrap();
    assert_eq!(rate.lines().count(), 1);
    assert!(rate.contains("over half of the 300"));
    assert!(rate.len() < 120, "{rate}");
}

#[test]
fn the_json_estimate_only_gains_additive_fields() {
    let json = serde_json::to_value(estimate(10, 300, None)).unwrap();
    for key in [
        "discovery",
        "checks",
        "deploy_polls",
        "total",
        "budget",
        "exceeds_budget",
    ] {
        assert!(json.get(key).is_some(), "{key}");
    }
    assert_eq!(json["warn"], false);
    assert!(json.get("rate_remaining").is_none());
    let known = serde_json::to_value(estimate(200, 300, Some(300))).unwrap();
    assert_eq!(known["rate_remaining"], 300);
    assert_eq!(known["warn"], true);
    assert_eq!(known["exceeds_budget"], false);
}
