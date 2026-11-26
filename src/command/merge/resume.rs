use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::{Execution, Hooks, Outcome, Settings, merge_plan_prepared, queued};
use crate::command::{PlanOptions, plan_discovered};
use crate::config::Config;
use crate::discovery::{Discovered, Scope, Skipped};
use crate::github::Client;
use crate::merge::{BumpWait, Kind, Report, Status, StepReport, Store, Summary, poll_delay};
use crate::model::{PrKey, PrState};
use crate::plan::Plan;

struct Selected {
    scope: Scope,
    base: String,
    head: String,
    sha: String,
}

#[derive(Default)]
pub(super) struct Selection(BTreeMap<String, Selected>);

impl Selection {
    pub(super) fn capture(found: &Discovered) -> Self {
        Self(
            found
                .ready
                .iter()
                .map(|(pr, scope)| (pr, *scope))
                .chain(found.skipped.iter().map(|s| (&s.pr, s.scope)))
                .map(|(pr, scope)| {
                    (
                        pr.key.to_string(),
                        Selected {
                            scope,
                            base: pr.base_ref.clone(),
                            head: pr.head_ref.clone(),
                            sha: pr.head_sha.clone(),
                        },
                    )
                })
                .collect(),
        )
    }

    fn refresh(
        &self,
        ids: &BTreeSet<String>,
        watched: &BTreeSet<String>,
        cfg: &Config,
        client: &mut Client,
    ) -> Result<(Discovered, Vec<StepReport>)> {
        let mut found = Discovered::default();
        let mut rejected = Vec::new();
        for id in ids {
            let selected = self.0.get(id).context("PR outside confirmed selection")?;
            let key = PrKey::parse(id).context("invalid selected PR")?;
            let pr = match client
                .pr(&key)
                .and_then(|pr| pr.context("selected PR is unavailable"))
            {
                Ok(pr) => pr,
                Err(error) => {
                    rejected.push(StepReport::new(
                        Kind::Merge,
                        id,
                        Status::Stopped,
                        &format!("cannot revalidate selected PR: {error:#}"),
                    ));
                    continue;
                }
            };
            if pr.key != key
                || pr.base_ref != selected.base
                || pr.head_ref != selected.head
                || (pr.head_sha != selected.sha && !watched.contains(id))
            {
                rejected.push(StepReport::new(Kind::Merge, id, Status::Stopped,
                    "the selected branch, base or head changed during the bump wait; inspect it before resuming"));
                continue;
            }
            let reasons = cfg.blockers(&pr);
            if reasons.is_empty() {
                found.ready.push((pr, selected.scope));
            } else {
                found.skipped.push(Skipped {
                    pr,
                    scope: selected.scope,
                    reasons,
                });
            }
        }
        Ok((found, rejected))
    }
}

fn transitioned(wait: &BumpWait, client: &mut Client) -> Result<bool> {
    match wait {
        BumpWait::Draft {
            key,
            base,
            head,
            sha,
        } => {
            let pr = client.rest(&format!("/repos/{}/pulls/{}", key.slug(), key.number))?;
            Ok(pr["state"] != "open"
                || pr["merged"] == true
                || pr["head"]["sha"] != sha.as_str()
                || pr["head"]["ref"] != head.as_str()
                || pr["base"]["ref"] != base.as_str())
        }
        BumpWait::Update { key, sha } => {
            let Some((pr, checked)) = client.pr_with_check_head(key)? else {
                return Ok(true);
            };
            Ok(pr.head_sha != *sha
                || pr.state != PrState::Open
                || pr.conflicting
                || (pr.checks == crate::model::Checks::Failure && checked.as_ref() == Some(sha))
                || pr.review == crate::model::Review::ChangesRequested)
        }
        BumpWait::Readiness { key, sha } => {
            let Some((pr, oid)) = client.pr_with_check_head(key)? else {
                return Ok(true);
            };
            Ok(pr.head_sha != *sha
                || !matches!(
                    queued::readiness_after_bump(&pr, oid.as_deref()),
                    Ok(Some(_))
                ))
        }
    }
}

fn waits(report: &Report) -> BTreeMap<BumpWait, BTreeSet<String>> {
    let mut waits = BTreeMap::<_, BTreeSet<_>>::new();
    for step in &report.steps {
        if let Some(wait) = &step.wait {
            waits
                .entry(wait.clone())
                .or_default()
                .insert(step.target.clone());
        }
    }
    waits
}

fn descendants(
    plan: &Plan,
    mut remaining: BTreeMap<String, BTreeSet<String>>,
    roots: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    remaining.retain(|id, _| {
        plan.items.iter().any(|item| !item.hidden && &item.id == id)
            || plan
                .skipped
                .iter()
                .any(|entry| !entry.hidden && &entry.id == id && !entry.blocked_by.is_empty())
    });
    let mut included = roots.clone();
    let mut downstream = BTreeMap::new();
    loop {
        let before = included.len();
        remaining.retain(|id, deps| {
            if !roots.contains(id) && !deps.is_disjoint(&included) {
                downstream.insert(id.clone(), deps.clone());
                included.insert(id.clone());
                false
            } else {
                true
            }
        });
        if included.len() == before {
            return downstream;
        }
    }
}

fn combine(outcome: &mut Outcome, next: Outcome, ids: &BTreeSet<String>) {
    let report = &mut outcome.report;
    report.steps.retain(|prior| {
        if ids.contains(&prior.target)
            && (prior.wait.is_some()
                || (prior.kind == Kind::Merge && prior.status == Status::Skipped))
        {
            return false;
        }
        !next.report.steps.iter().any(|step| {
            step.kind == prior.kind
                && step.target == prior.target
                && step.bump.as_ref().map(|b| &b["package"])
                    == prior.bump.as_ref().map(|b| &b["package"])
        })
    });
    report.steps.extend(next.report.steps);
    report.not_merging.retain(|s| !ids.contains(&s.id));
    report.not_merging.extend(next.report.not_merging);
    report.preflight.retain(|p| !ids.contains(&p.id));
    report.preflight.extend(next.report.preflight);
    report.questions = next.report.questions;
    report.stopped = next.report.stopped;
    report.api_usage = next.report.api_usage;
    report.summary = Summary::from_steps(&report.steps);
    outcome.code = if report.stopped.is_some() {
        next.code
    } else {
        report.exit_code()
    };
}

#[allow(clippy::too_many_arguments)]
pub(super) fn follow(
    outcome: &mut Outcome,
    selection: Selection,
    execution: &Execution,
    options: &PlanOptions,
    cfg: &Config,
    client: &mut Client,
    settings: &Settings,
    store: &mut Store,
    hooks: &mut Hooks,
    compact: bool,
) -> Result<()> {
    let initial = execution.selection;
    let mut watches = waits(&outcome.report);
    if watches.is_empty() {
        return Ok(());
    }
    let mut watched: BTreeSet<_> = watches.values().flatten().cloned().collect();
    let prerequisites: BTreeMap<String, BTreeSet<String>> = initial
        .selected_dependencies
        .iter()
        .map(|(id, edges)| {
            (
                id.clone(),
                edges.iter().map(|edge| edge.id.clone()).collect(),
            )
        })
        .collect();
    let mut downstream = descendants(initial, prerequisites.clone(), &watched);
    let start = (hooks.now)();
    let timeout = settings.timeout.unwrap_or(Duration::from_secs(
        cfg.merge.timeout_minutes.saturating_mul(60),
    ));
    let interval = Duration::from_secs(cfg.merge.poll_seconds.max(1));
    let ceiling = Duration::from_secs(cfg.merge.poll_max_seconds);
    let mut sleeps = 0;
    while outcome.report.stopped.is_none() {
        let merged: BTreeSet<_> = outcome
            .report
            .steps
            .iter()
            .filter(|s| s.merge_sha.is_some() && s.status == Status::Done)
            .map(|s| s.target.clone())
            .collect();
        let pending: BTreeSet<_> = watches
            .values()
            .flatten()
            .chain(downstream.keys())
            .collect();
        for step in outcome
            .report
            .steps
            .iter_mut()
            .filter(|step| step.wait.is_some())
        {
            if let Some(blocker) = prerequisites
                .get(&step.target)
                .into_iter()
                .flatten()
                .find(|id| !merged.contains(*id) && !pending.contains(*id))
            {
                step.wait = None;
                step.blocked_by = Some(blocker.clone());
                step.detail
                    .push_str(&format!("; prerequisite {blocker} did not complete"));
            }
        }
        watches = waits(&outcome.report);
        let mut ready = BTreeSet::new();
        downstream.retain(|id, deps| {
            if deps.is_subset(&merged) {
                ready.insert(id.clone());
                false
            } else {
                true
            }
        });
        if ready.is_empty() {
            if watches.is_empty() {
                break;
            }
            let remaining = timeout.saturating_sub((hooks.now)().duration_since(start));
            if remaining.is_zero() {
                outcome.report.stopped =
                    Some("bump wait timed out; resume with the same selection and state".into());
                break;
            }
            if sleeps == 0 {
                for (wait, consumers) in &watches {
                    let ids = consumers.iter().cloned().collect::<Vec<_>>().join(", ");
                    let label = match wait {
                        BumpWait::Draft { key, .. } => format!(
                            "human integration of https://github.com/{}/pull/{}",
                            key.slug(),
                            key.number
                        ),
                        BumpWait::Update { key, .. } => format!("branch update of {key}"),
                        BumpWait::Readiness { key, sha } => {
                            format!("approval and CI on {key} at {sha}")
                        }
                    };
                    eprintln!("Waiting for {label} before continuing {ids} automatically.");
                }
            }
            (hooks.sleep)(poll_delay(interval, ceiling, sleeps).min(remaining));
            sleeps += 1;
            if let Some(below) = client.below_reserve() {
                outcome.report.stopped = Some(crate::quota::stop_message(&below));
                outcome.code = crate::quota::EXIT_QUOTA;
                break;
            }
            for (wait, ids) in &watches {
                match transitioned(wait, client) {
                    Ok(true) => {
                        for id in ids {
                            if prerequisites
                                .get(id)
                                .is_none_or(|deps| deps.is_subset(&merged))
                            {
                                ready.insert(id.clone());
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        for step in outcome
                            .report
                            .steps
                            .iter_mut()
                            .filter(|step| step.wait.as_ref() == Some(wait))
                        {
                            step.wait = None;
                            step.detail
                                .push_str(&format!("; cannot monitor this PR: {error:#}"));
                        }
                    }
                }
            }
            if ready.is_empty() {
                watches = waits(&outcome.report);
                continue;
            }
        }
        sleeps = 0;
        let (mut found, rejected) = selection.refresh(&ready, &watched, cfg, client)?;
        if initial.train == initial.deploy_branch {
            crate::discovery::refresh_questions(&mut found, &initial.deploy_branch);
        }
        let keys = ready
            .iter()
            .filter_map(|id| PrKey::parse(id))
            .collect::<Vec<_>>();
        let phase = plan_discovered(
            found,
            options,
            cfg,
            client,
            &keys,
            initial.train.clone(),
            initial.deploy_branch.clone(),
            initial.profile.clone().context("missing merge profile")?,
            initial.shown_authors.as_deref().unwrap_or_default(),
            Instant::now(),
        )?;
        let mut next = merge_plan_prepared(
            &phase,
            cfg,
            client,
            settings,
            store,
            hooks,
            &BTreeMap::new(),
            execution,
            compact,
        )?;
        next.report.steps.extend(rejected);
        for id in &ready {
            if !next.report.steps.iter().any(|step| step.target == *id)
                && let Some(entry) = next.report.not_merging.iter().find(|entry| entry.id == *id)
            {
                next.report.steps.push(StepReport::new(
                    Kind::Merge,
                    id,
                    Status::Stopped,
                    &entry.reasons.join(", "),
                ));
            }
        }
        combine(outcome, next, &ready);
        watches = waits(&outcome.report);
        watched.extend(watches.values().flatten().cloned());
    }
    outcome
        .report
        .steps
        .sort_by_key(|step| step.kind == Kind::Bump && step.bump.is_some());
    outcome.report.api_usage = client.usage();
    Ok(())
}
