use std::time::Duration;

use super::{Env, Hooks};
use crate::bundle::is_stacked;
use crate::deploy_wait::{DeployClock, waiting_for_runner};
use crate::github::Client;
use crate::merge::{DeployOutcome, Status, StepReport, Verdict, judge, judge_expected};
use crate::workflows::{self, PushRuns};

struct Limits {
    timeout: Duration,
    queued_ceiling: Duration,
    poll: Duration,
    poll_ceiling: Duration,
    grace: Duration,
}

enum Deploy {
    Passed(usize),
    Skipped(usize),
    Failed(String),
    TimedOut(String),
    QueuedTooLong(String),
    NoRun(Duration),
    Unreadable(String),
}

fn wait_for_deploy(
    client: &mut Client,
    slug: &str,
    sha: &str,
    limits: &Limits,
    hooks: &mut Hooks,
    prediction: Option<&PushRuns>,
    report: &mut StepReport,
) -> (Deploy, Vec<String>) {
    let expected = prediction.filter(|value| matches!(value, PushRuns::Expected { .. }));
    let workflows = match expected {
        Some(PushRuns::Expected {
            workflows,
            chained_workflows,
            ..
        }) => workflows
            .iter()
            .chain(chained_workflows)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        _ => "legacy workflow runs".to_string(),
    };
    let mut emit =
        |message: &str| (hooks.progress)(&format!("waiting for deploy: {workflows}; {message}"));
    let poll = limits.poll.max(Duration::from_millis(1));
    let mut clock = DeployClock::new((hooks.now)(), limits.timeout, limits.queued_ceiling);
    let mut sleeps = 0u64;
    let mut consecutive_errors = 0;
    let mut settled: Option<usize> = None;
    let mut last = "no workflow run yet".to_string();
    let mut auxiliary_failures = std::collections::BTreeSet::new();
    let mut known: Vec<crate::github::Run> = Vec::new();
    let outcome = loop {
        let now = (hooks.now)();
        if clock.ceiling_hit(now) {
            break Deploy::QueuedTooLong(last);
        }
        if clock.timed_out(now) {
            break Deploy::TimedOut(last);
        }
        let reading = if follow_by_id(expected, &known) {
            client.refresh_runs(slug, &known).map(|runs| (runs, true))
        } else {
            client.workflow_runs(slug, sha, expected.is_none())
        };
        let (runs, complete) = match reading {
            Ok(reading) => {
                consecutive_errors = 0;
                known = reading.0.clone();
                reading
            }
            Err(error) if transient_read(&error) => {
                consecutive_errors += 1;
                settled = None;
                last = format!("transient workflow read error: {error}");
                let remaining = clock.remaining((hooks.now)());
                if remaining.is_zero() {
                    break if clock.queued() {
                        Deploy::QueuedTooLong(last)
                    } else {
                        Deploy::TimedOut(last)
                    };
                }
                if consecutive_errors > 3 {
                    break Deploy::Unreadable(format!("{error}; exhausted 3 consecutive retries"));
                }
                emit(&format!("{last}; retry {consecutive_errors}/3"));
                (hooks.sleep)(poll.min(remaining));
                continue;
            }
            Err(error) => break Deploy::Unreadable(error.to_string()),
        };
        let now = (hooks.now)();
        let waiting = {
            let tracked: Vec<&crate::github::Run> = match expected {
                Some(PushRuns::Expected {
                    workflows,
                    chained_workflows,
                    ..
                }) => runs
                    .iter()
                    .filter(|run| {
                        workflows.contains(&run.path) || chained_workflows.contains(&run.path)
                    })
                    .collect(),
                _ => runs.iter().collect(),
            };
            waiting_for_runner(&tracked)
        };
        if waiting != clock.queued() {
            if waiting {
                clock.observe(now, true);
                if let Some(runner) = hooks.runner.as_mut() {
                    runner(true);
                }
                emit("deploy is queued, waiting for a runner");
            } else {
                if let Some(runner) = hooks.runner.as_mut() {
                    runner(false);
                }
                clock.observe((hooks.now)(), false);
            }
        }
        let now = (hooks.now)();
        if clock.ceiling_hit(now) {
            break Deploy::QueuedTooLong(last);
        }
        if clock.timed_out(now) {
            break Deploy::TimedOut(last);
        }
        let elapsed = clock.elapsed(now);
        let observed = runs
            .iter()
            .map(|run| format!("{}: {}", run.name, run.url))
            .collect::<Vec<_>>()
            .join("; ");
        emit(&format!(
            "deploy {slug} at SHA {sha}: {}",
            if observed.is_empty() {
                "no run yet"
            } else {
                &observed
            }
        ));
        let mut verdict = if let Some(prediction) = expected {
            if let PushRuns::Expected {
                workflows,
                chained_workflows,
                ..
            } = prediction
            {
                let is_expected =
                    |path: &String| workflows.contains(path) || chained_workflows.contains(path);
                report.expected_runs = runs
                    .iter()
                    .filter(|run| is_expected(&run.path))
                    .map(crate::merge::ExpectedRun::from)
                    .collect();
                for run in runs.iter().filter(|run| !is_expected(&run.path)) {
                    if let Verdict::Failed(text) = judge(std::slice::from_ref(run)) {
                        auxiliary_failures
                            .insert(format!("auxiliary {text} (does not decide deploy)"));
                    }
                }
            }
            judge_expected(&runs, prediction)
        } else {
            judge(&runs)
        };
        if !complete && !matches!(verdict, Verdict::Failed(_)) {
            verdict = Verdict::Waiting(
                "workflow run response is incomplete; cannot confirm deploy".to_string(),
            );
        }
        let no_runs = matches!(verdict, Verdict::NoRuns);
        match verdict {
            Verdict::Failed(text) => break Deploy::Failed(text),
            verdict @ (Verdict::Passed(count) | Verdict::Skipped(count)) => {
                let skipped = matches!(verdict, Verdict::Skipped(_));
                if settled == Some(count) {
                    break if skipped {
                        Deploy::Skipped(count)
                    } else {
                        Deploy::Passed(count)
                    };
                }
                settled = Some(count);
                let conclusion = if skipped { "skipped" } else { "passed" };
                emit(&format!(
                    "deploy {slug}: {count} workflows {conclusion}, confirming completion"
                ));
            }
            Verdict::Waiting(text) => {
                settled = None;
                emit(&format!("deploy {slug}: {text}"));
                last = format!("{text}; SHA {sha}; {observed}");
            }
            Verdict::NoRuns => {
                settled = None;
                emit(&format!("deploy {slug}: waiting for a workflow to start"));
                if elapsed >= limits.grace {
                    break Deploy::NoRun(limits.grace);
                }
            }
        }
        let remaining = clock.remaining((hooks.now)());
        let remaining = if no_runs {
            remaining.min(limits.grace.saturating_sub(elapsed))
        } else {
            remaining
        };
        let delay = crate::merge::poll_delay(poll, limits.poll_ceiling, sleeps);
        let delay = match client.below_reserve() {
            Some(below) => {
                let slow = crate::quota::backoff(delay, limits.poll_ceiling, &below, sleeps);
                emit(&format!(
                    "GitHub {} quota is below the reserve ({} left of {}): following the deploy every {}s",
                    below.resource,
                    below.left,
                    below.floor,
                    slow.as_secs()
                ));
                slow
            }
            None => delay,
        };
        (hooks.sleep)(delay.min(remaining));
        sleeps += 1;
    };
    if clock.queued() {
        clock.observe((hooks.now)(), false);
        if let Some(runner) = hooks.runner.as_mut() {
            runner(false);
        }
    }
    (outcome, auxiliary_failures.into_iter().collect())
}

fn follow_by_id(expected: Option<&PushRuns>, known: &[crate::github::Run]) -> bool {
    let Some(PushRuns::Expected {
        workflows,
        chained_workflows,
        ..
    }) = expected
    else {
        return false;
    };
    chained_workflows.is_empty()
        && workflows
            .iter()
            .all(|path| known.iter().any(|run| &run.path == path))
        && known.iter().all(|run| run.id != 0)
        && known.iter().any(|run| run.status != "completed")
}

fn transient_read(error: &crate::github::ApiError) -> bool {
    match error {
        crate::github::ApiError::Network => true,
        crate::github::ApiError::Other { message } => message
            .strip_prefix("GitHub answered ")
            .and_then(|text| text.split_whitespace().next())
            .and_then(|status| status.parse::<u16>().ok())
            .is_some_and(|status| (500..600).contains(&status)),
        _ => false,
    }
}

pub(super) fn track(
    env: &Env,
    client: &mut Client,
    hooks: &mut Hooks,
    slug: &str,
    base: &str,
    sha: &str,
    report: &mut StepReport,
) -> (Status, String) {
    (hooks.progress)(&format!("checking deploy workflows for {slug}@{base}"));
    if !env.cfg.merge.waits_for_deploy(slug) {
        report.deploy_outcome = Some(DeployOutcome::Dispensed);
        return (
            Status::Done,
            "not waited: the repo is listed in merge.no_deploy_repos".to_string(),
        );
    }
    let saved_prediction = report.deploy_prediction.clone();
    let prediction = saved_prediction.as_ref();
    let legacy = if prediction.is_none() && is_stacked(base, &env.cfg.train_prefix) {
        Some(match workflows::read(client, slug, sha) {
            Ok(files) => workflows::branch_push_runs(&files, base),
            Err(error) => PushRuns::indeterminate(format!("{error:#}")),
        })
    } else {
        None
    };
    let display_prediction = prediction.or(legacy.as_ref());
    let note = display_prediction.map(|reading| reading.detail(base));
    if matches!(display_prediction, Some(PushRuns::Dispensed { .. })) {
        report.deploy_outcome = Some(DeployOutcome::Dispensed);
        return (
            Status::Done,
            format!("not waited: {}", note.unwrap_or_default()),
        );
    }
    let timeout = env
        .settings
        .timeout
        .unwrap_or_else(|| env.cfg.merge.timeout_for(slug));
    let limits = Limits {
        timeout,
        queued_ceiling: env.cfg.merge.queued_ceiling(),
        poll: Duration::from_secs(env.cfg.merge.poll_seconds.max(1)),
        poll_ceiling: Duration::from_secs(env.cfg.merge.poll_max_seconds),
        grace: Duration::from_secs(env.cfg.merge.no_run_grace_seconds),
    };
    let (outcome, auxiliary_failures) =
        wait_for_deploy(client, slug, sha, &limits, hooks, prediction, report);
    report.deploy_outcome = Some(match &outcome {
        Deploy::Passed(_) if matches!(display_prediction, Some(PushRuns::Indeterminate { .. })) => {
            DeployOutcome::Indeterminate
        }
        Deploy::Passed(_) => DeployOutcome::Confirmed,
        Deploy::Skipped(_) => DeployOutcome::Skipped,
        Deploy::Failed(_) => DeployOutcome::Failed,
        _ => DeployOutcome::Indeterminate,
    });
    let said = |text: String| {
        let mut parts: Vec<String> = note.iter().cloned().collect();
        parts.push(format!("SHA {sha}: {text}"));
        parts.extend(report.expected_runs.iter().map(|run| {
            format!(
                "expected workflow {} ({}): {}",
                run.name, run.path, run.html_url
            )
        }));
        parts.extend(auxiliary_failures.iter().cloned());
        parts.join("; ")
    };
    match outcome {
        Deploy::Passed(count) => (
            Status::Done,
            said(format!("deploy passed ({count} workflow runs)")),
        ),
        Deploy::Skipped(count) => (
            Status::Skipped,
            said(format!(
                "deploy skipped (all {count} workflow runs were skipped)"
            )),
        ),
        Deploy::Failed(text) => (Status::Failed, said(format!("deploy failed: {text}"))),
        Deploy::TimedOut(last) => (
            Status::Stopped,
            said(format!(
                "still deploying after {} minutes ({last}); run again to keep waiting",
                timeout.as_secs() / 60
            )),
        ),
        Deploy::QueuedTooLong(last) => (
            Status::Stopped,
            said(format!(
                "deploy still queued for a runner after {} minutes ({last}); run again to keep waiting",
                limits.queued_ceiling.as_secs() / 60
            )),
        ),
        Deploy::NoRun(grace) => (
            Status::Stopped,
            said(format!(
                "no workflow run appeared for the push in {}s; run again to check once more",
                grace.as_secs()
            )),
        ),
        Deploy::Unreadable(error) => (
            Status::Stopped,
            said(format!(
                "could not read the workflow runs ({error}); run again to resume"
            )),
        ),
    }
}
