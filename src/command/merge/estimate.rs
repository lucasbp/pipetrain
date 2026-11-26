use std::time::Duration;

use super::{Settings, planned_steps};
use crate::config::Config;
use crate::merge::{ApiEstimate, Kind, StepReport, Store, estimated_polls};
use crate::plan::Plan;
use crate::workflows::PushRuns;

fn deploy_polls(cfg: &Config, settings: &Settings, slug: &str) -> u64 {
    let timeout = settings
        .timeout
        .unwrap_or_else(|| cfg.merge.timeout_for(slug));
    estimated_polls(
        timeout,
        Duration::from_secs(cfg.merge.poll_seconds.max(1)),
        Duration::from_secs(cfg.merge.poll_max_seconds),
    )
}

pub(super) fn requests(
    plan: &Plan,
    cfg: &Config,
    settings: &Settings,
    store: &Store,
    requests: u32,
    evaluated: &[StepReport],
) -> ApiEstimate {
    let mut discovery = u64::from(requests.max(plan.stats.requests));
    let mut checks = 0u64;
    let mut polls = 0u64;
    for step in planned_steps(plan, cfg) {
        if step.kind != Kind::Merge {
            continue;
        }
        let Some(item) = plan.items.iter().find(|item| item.id == step.target) else {
            continue;
        };
        let slug = format!("{}/{}", item.org, item.repo);
        checks = checks.saturating_add(3);
        if plan.items.iter().any(|child| {
            child.org.eq_ignore_ascii_case(&item.org)
                && child.repo.eq_ignore_ascii_case(&item.repo)
                && child.base == item.head
        }) {
            checks = checks.saturating_add(3);
        }
        if cfg.merge.waits_for_deploy(&slug) {
            discovery = discovery.saturating_add(7);
            if !evaluated.iter().any(|step| {
                step.target == item.id
                    && matches!(step.deploy_prediction, Some(PushRuns::Dispensed { .. }))
            }) {
                polls = polls.saturating_add(deploy_polls(cfg, settings, &slug));
            }
        }
    }
    for pending in &store.state.pending {
        checks = checks.saturating_add(4 + u64::from(pending.sha.is_none()));
        if cfg.merge.waits_for_deploy(&pending.repo)
            && !matches!(pending.deploy_prediction, Some(PushRuns::Dispensed { .. }))
        {
            polls = polls.saturating_add(deploy_polls(cfg, settings, &pending.repo));
        }
    }
    let total = discovery.saturating_add(checks).saturating_add(polls);
    let exceeds_budget = total > u64::from(cfg.api.max_requests);
    let rate_remaining = plan.stats.rate_remaining;
    ApiEstimate {
        discovery,
        checks,
        deploy_polls: polls,
        total,
        budget: cfg.api.max_requests,
        exceeds_budget,
        rate_remaining,
        warn: exceeds_budget || rate_remaining.is_some_and(|left| total > left / 2),
    }
}
