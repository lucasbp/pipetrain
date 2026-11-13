use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use super::apply::{Gate, deploy_pr_body, gate, tint};
use super::build_plan_with_config;
use super::order::requests_line;
use crate::bundle::{base_suffix, is_stacked, is_train_branch};
use crate::command::PlanOptions;
use crate::config::Config;
use crate::emit;
use crate::github::{Client, LivePr, PrWrite};
use crate::merge::{
    DependencyFailure, DeployOutcome, Kind, NotMerging, Pending, REPORT_SCHEMA, Report, STOPPED,
    State, Status, Step, StepReport, Store, Summary, Verdict, judge, judge_expected,
    resolve_state_path, steps,
};
use crate::model::{PrKey, PrState};
use crate::plan::Plan;
use crate::planner::Item;
use crate::style::{bold, dim, green, red, yellow};
use crate::workflows::{self, PushRuns};

pub mod bump;
mod failure;
mod package;
mod parallel;
mod recovery;
mod stacked;

const MERGEABILITY_TRIES: usize = 13;
const MERGEABILITY_PAUSE: Duration = Duration::from_secs(5);
const TRAIN_NOTICE: &str =
    "intermediate train branch; promotion to the release target is still required";
const METHODS: [&str; 3] = ["merge", "squash", "rebase"];

pub struct MergeOptions {
    pub plan: PlanOptions,
    pub apply: bool,
    pub yes: bool,
    pub gates_confirmed: bool,
    pub allow_questions: bool,
    pub method: Option<String>,
    pub timeout_minutes: Option<u64>,
    pub state: Option<PathBuf>,
    pub open_pr: bool,
    pub dependency_failure: DependencyFailure,
    pub bump: bool,
    pub bump_options: bump::Options,
}

pub struct Settings {
    pub apply: bool,
    pub gates_confirmed: bool,
    pub allow_questions: bool,
    pub method: Option<String>,
    pub timeout: Option<Duration>,
    pub feed_registry: Option<String>,
    pub open_pr: bool,
    pub dependency_failure: DependencyFailure,
    pub bump: bool,
    pub bump_options: bump::Options,
}

pub struct Hooks<'a> {
    pub confirm: &'a mut dyn FnMut(&str) -> bool,
    pub gate: &'a mut dyn FnMut(&str) -> bool,
    pub sleep: &'a mut dyn FnMut(Duration),
    pub observe: &'a mut dyn FnMut(&StepReport),
    pub progress: &'a mut dyn FnMut(&str),
}

#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    pub aborted: bool,
    pub code: i32,
}

type PushReadingKey = (String, String, String, Vec<String>);

struct Env<'a> {
    plan: &'a Plan,
    cfg: &'a Config,
    settings: &'a Settings,
    method: &'a str,
    readings: RefCell<BTreeMap<PushReadingKey, PushRuns>>,
    stacked: stacked::Evidence,
}

impl Env<'_> {
    fn push_reading(
        &self,
        client: &mut Client,
        key: &PrKey,
        branch: &str,
        sha: &str,
        head_sha: &str,
    ) -> PushRuns {
        if !self.cfg.merge.waits_for_deploy(&key.slug()) {
            return PushRuns::Dispensed {
                workflows: Vec::new(),
                reason: "the repo is listed in merge.no_deploy_repos".to_string(),
            };
        }
        let changed = match client.push_files(key, head_sha) {
            Ok(changed) => changed,
            Err(error) => return PushRuns::indeterminate(format!("{error:#}")),
        };
        let slug = key.slug();
        let cache_key = (
            slug.to_lowercase(),
            branch.to_string(),
            sha.to_string(),
            changed.clone(),
        );
        if let Some(known) = self.readings.borrow().get(&cache_key) {
            return known.clone();
        }
        let reading = match workflows::read(client, &slug, sha) {
            Ok(files) => workflows::push_runs(&files, branch, &changed),
            Err(error) => PushRuns::indeterminate(format!("{error:#}")),
        };
        self.readings
            .borrow_mut()
            .insert(cache_key, reading.clone());
        reading
    }
}

struct Limits {
    timeout: Duration,
    poll: Duration,
    grace: Duration,
}

enum Deploy {
    Passed(usize),
    Skipped(usize),
    Failed(String),
    TimedOut(String),
    NoRun(Duration),
    Unreadable(String),
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn progress_target(plan: &Plan, state: &State, step: &StepReport) -> String {
    let base = match step.kind {
        Kind::Merge => plan
            .items
            .iter()
            .find(|item| item.id == step.target)
            .map(|item| item.base.as_str()),
        Kind::Resume => state
            .pending
            .iter()
            .find(|pending| pending.id == step.target)
            .map(|pending| pending.base.as_str()),
        _ => None,
    };
    match base {
        Some(base) => format!("{} → {base}", step.target),
        None => step.target.clone(),
    }
}

fn method_of(settings: &Settings, cfg: &Config) -> Result<String> {
    let method = settings
        .method
        .clone()
        .unwrap_or_else(|| cfg.merge.method.clone())
        .to_lowercase();
    if METHODS.contains(&method.as_str()) {
        Ok(method)
    } else {
        bail!("the merge method `{method}` is not one of merge, squash, rebase")
    }
}

fn planned_steps(plan: &Plan, cfg: &Config) -> Vec<StepReport> {
    steps(&emit::queue_lines(plan))
        .into_iter()
        .map(|step| match step {
            Step::Merge(id) => {
                let detail = plan
                    .items
                    .iter()
                    .find(|item| item.id == id)
                    .map(|item| {
                        let mut detail = format!("into {}", item.base);
                        if let Some(chain) = crate::planner::stacked_chain(item, &plan.items) {
                            detail.push_str(&format!(
                                "; stack: {chain}; {}",
                                crate::planner::STACKED_STRATEGY
                            ));
                        }
                        if is_stacked(&item.base, &cfg.train_prefix) {
                            detail.push_str(&format!("; {TRAIN_NOTICE}"));
                        }
                        detail
                    })
                    .unwrap_or_default();
                StepReport::new(Kind::Merge, &id, Status::Planned, &detail)
            }
            Step::Gate(text) => StepReport::new(Kind::Gate, &text, Status::Planned, ""),
            Step::Wait(text) => StepReport::new(Kind::Wait, &text, Status::Planned, ""),
        })
        .collect()
}

fn resume_steps(state: &State) -> Vec<StepReport> {
    state
        .pending
        .iter()
        .map(|pending| {
            let mut step = StepReport::new(
                Kind::Resume,
                &pending.id,
                Status::Planned,
                "merged by an earlier run, waiting for its deploy",
            );
            step.deploy_prediction = pending.deploy_prediction.clone();
            step
        })
        .collect()
}

fn not_merging(plan: &Plan) -> Vec<NotMerging> {
    plan.skipped
        .iter()
        .filter(|skipped| !skipped.hidden)
        .map(|skipped| NotMerging {
            id: skipped.id.clone(),
            reasons: skipped.reasons.clone(),
        })
        .collect()
}

fn fail(report: &mut StepReport, detail: impl Into<String>) {
    report.status = Status::Failed;
    report.detail = detail.into();
}

fn stop(report: &mut StepReport, detail: impl Into<String>) {
    report.status = Status::Stopped;
    report.detail = detail.into();
}

fn not_clean(state: &str) -> Option<String> {
    if state == "clean" {
        return None;
    }
    if let Some((cause, next)) = crate::model::merge_state_blocker(state) {
        return Some(format!("{cause}: {next}"));
    }
    Some(match state {
        "unknown" | "" => {
            "GitHub has not finished working out mergeability: run again in a minute".to_string()
        }
        other => format!("GitHub reports the state `{other}`, not clean"),
    })
}

fn settled_live(
    client: &mut Client,
    slug: &str,
    number: u64,
    hooks: &mut Hooks,
) -> Result<LivePr, crate::github::ApiError> {
    let mut tries = 0;
    loop {
        let live = client.live_pr(slug, number)?;
        tries += 1;
        let unsettled = live.open && matches!(live.mergeable_state.as_str(), "unknown" | "");
        if !unsettled || tries >= MERGEABILITY_TRIES {
            return Ok(live);
        }
        (hooks.progress)(&format!(
            "GitHub is recalculating mergeability of {slug}#{number}; retry {tries}/{} in 5s",
            MERGEABILITY_TRIES - 1
        ));
        (hooks.sleep)(MERGEABILITY_PAUSE);
    }
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
    let mut waited = Duration::ZERO;
    let mut settled: Option<usize> = None;
    let mut last = "no workflow run yet".to_string();
    let mut auxiliary_failures = std::collections::BTreeSet::new();
    let outcome = loop {
        let (runs, complete) = match client.workflow_runs(slug, sha, expected.is_none()) {
            Ok(reading) => reading,
            Err(error) => break Deploy::Unreadable(error.to_string()),
        };
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
                if waited >= limits.grace {
                    break Deploy::NoRun(limits.grace);
                }
            }
        }
        if waited >= limits.timeout {
            break Deploy::TimedOut(last);
        }
        (hooks.sleep)(poll);
        waited += poll;
    };
    (outcome, auxiliary_failures.into_iter().collect())
}

fn deploy_phase(
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
        poll: Duration::from_secs(env.cfg.merge.poll_seconds.max(1)),
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

fn confirm_merge(
    client: &mut Client,
    slug: &str,
    number: u64,
) -> Result<Option<String>, crate::github::ApiError> {
    let live = client.live_pr(slug, number)?;
    Ok(live.merged.then_some(live.merge_commit_sha).flatten())
}

fn merge_one(
    env: &Env,
    item: &Item,
    client: &mut Client,
    store: &mut Store,
    hooks: &mut Hooks,
    report: &mut StepReport,
) {
    let key = PrKey::new(&item.org, &item.repo, item.number);
    let slug = key.slug();
    (hooks.progress)(&format!("checking merge readiness of {}", item.id));
    let mut tries = 0;
    let pr = loop {
        let pr = match client.pr(&key) {
            Ok(Some(pr)) => pr,
            Ok(None) => return fail(report, "not found on GitHub"),
            Err(error) => return fail(report, format!("{error:#}")),
        };
        tries += 1;
        let mut settled = pr.clone();
        settled.mergeable_unknown = false;
        if !(pr.mergeable_unknown || pr.merge_state_status.eq_ignore_ascii_case("UNKNOWN"))
            || !env.cfg.blockers(&settled).is_empty()
        {
            break pr;
        }
        if tries >= MERGEABILITY_TRIES {
            return stop(
                report,
                "GitHub mergeability is still unknown after 60s; no merge was attempted, run again to resume",
            );
        }
        (hooks.progress)(&format!(
            "GitHub is recalculating mergeability of {}; retry {tries}/{} in 5s",
            item.id,
            MERGEABILITY_TRIES - 1
        ));
        (hooks.sleep)(MERGEABILITY_PAUSE);
    };
    if pr.state == PrState::Merged {
        report.status = Status::Skipped;
        report.detail = "already merged".to_string();
        return;
    }
    let reasons = env.cfg.blockers(&pr);
    if !reasons.is_empty() {
        let readiness = pr.readiness(reasons);
        if env.stacked.has_child(item) {
            return stop(
                report,
                format!(
                    "no longer ready after its stacked child landed: {}; next: {}; re-approve {} after its stacked child landed",
                    readiness.causes.join(", "),
                    readiness.next_step,
                    item.id
                ),
            );
        }
        return fail(
            report,
            format!(
                "no longer ready: {}; next: {}",
                readiness.causes.join(", "),
                readiness.next_step
            ),
        );
    }
    let live = match settled_live(client, &slug, item.number, hooks) {
        Ok(live) => live,
        Err(error) => return fail(report, error.to_string()),
    };
    if live.merged {
        report.status = Status::Skipped;
        report.detail = "already merged".to_string();
        return;
    }
    if !live.open {
        return fail(report, "it is closed");
    }
    if live.head_sha != item.sha || pr.head_sha != item.sha {
        let accepted = pr.head_sha == live.head_sha
            && match env.stacked.accepts(item, client, &live.head_sha) {
                Ok(accepted) => accepted,
                Err(error) => {
                    return fail(report, format!("cannot verify stacked head: {error:#}"));
                }
            };
        if !accepted {
            return fail(
                report,
                format!(
                    "the branch moved after the plan ({} to {}): run the plan again",
                    short(&item.sha),
                    short(&live.head_sha)
                ),
            );
        }
    }
    if live.base != item.base {
        return fail(
            report,
            format!(
                "the base changed after the plan ({} to {}): run the plan again",
                item.base, live.base
            ),
        );
    }
    if let Some(why) = not_clean(&live.mergeable_state) {
        if matches!(live.mergeable_state.as_str(), "unknown" | "") {
            return stop(report, why);
        }
        return fail(report, why);
    }
    let pending = Pending {
        recorded_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        id: item.id.clone(),
        repo: slug.clone(),
        base: item.base.clone(),
        sha: None,
        continuation: Some(failure::continuation(env.plan, item)),
        deploy_prediction: None,
    };
    if let Err(error) = store.add(pending) {
        return fail(report, format!("cannot save the merge state: {error:#}"));
    }
    (hooks.progress)(&format!("merging {} into {}", item.id, item.base));
    let merge_sha = match client.merge_pr(&slug, item.number, &live.head_sha, env.method) {
        Ok(sha) => sha,
        Err(error) => match confirm_merge(client, &slug, item.number) {
            Ok(Some(sha)) => sha,
            Ok(None) => {
                let _ = store.remove(&item.id);
                return fail(report, error.to_string());
            }
            Err(_) => {
                return stop(
                    report,
                    format!("{error}; could not confirm whether it merged, run again to find out"),
                );
            }
        },
    };
    report.merge_sha = Some(merge_sha.clone());
    (hooks.progress)(&format!("merged {} at SHA {merge_sha}", item.id));
    if let Err(error) = store.set_sha(&item.id, &merge_sha) {
        return stop(
            report,
            format!(
                "merged {}, but the merge state could not be saved: {error:#}",
                short(&merge_sha)
            ),
        );
    }
    env.stacked.record(item, &live.head_sha, &merge_sha);
    if env.settings.open_pr
        && is_stacked(&item.base, &env.cfg.train_prefix)
        && let Err(error) = store.add_deploy_pr(&slug, &item.base, &item.id)
    {
        return stop(
            report,
            format!(
                "merged {}, but the deploy PR could not be recorded: {error:#}",
                short(&merge_sha)
            ),
        );
    }
    report.deploy_prediction = Some(PushRuns::Dispensed {
        workflows: Vec::new(),
        reason: "the repo is listed in merge.no_deploy_repos".to_string(),
    });
    if env.cfg.merge.waits_for_deploy(&slug) {
        (hooks.progress)(&format!(
            "checking deploy workflows for {slug}@{}",
            item.base
        ));
        let prediction = env.push_reading(client, &key, &item.base, &merge_sha, &live.head_sha);
        report.deploy_prediction = Some(prediction.clone());
        if let Some(mut pending) = store
            .state
            .pending
            .iter()
            .find(|pending| pending.id == item.id)
            .cloned()
        {
            pending.deploy_prediction = Some(prediction);
            if let Err(error) = store.add(pending) {
                return stop(
                    report,
                    format!("cannot save the deploy prediction: {error:#}"),
                );
            }
        }
    }
    let landed = Landed {
        id: &item.id,
        slug: &slug,
        base: &item.base,
        sha: &merge_sha,
    };
    finish_deploy(env, &landed, client, store, hooks, report);
}

#[derive(Clone, Copy)]
struct Landed<'a> {
    id: &'a str,
    slug: &'a str,
    base: &'a str,
    sha: &'a str,
}

fn finish_deploy(
    env: &Env,
    landed: &Landed,
    client: &mut Client,
    store: &mut Store,
    hooks: &mut Hooks,
    report: &mut StepReport,
) {
    let Landed {
        id,
        slug,
        base,
        sha,
    } = *landed;
    let (status, text) = deploy_phase(env, client, hooks, slug, base, sha, report);
    report.deploy = Some(text);
    report.status = status;
    report.detail = match status {
        Status::Done => format!("merged {} into {base}", short(sha)),
        Status::Skipped => format!("merged {} into {base}, deploy skipped", short(sha)),
        Status::Failed => format!("merged {} into {base}, but the deploy failed", short(sha)),
        _ => format!(
            "merged {} into {base}, but the deploy did not finish",
            short(sha)
        ),
    };
    if is_stacked(base, &env.cfg.train_prefix) {
        report.detail.push_str(&format!("; {TRAIN_NOTICE}"));
    }
    if matches!(status, Status::Done | Status::Skipped)
        && let Err(error) = store.remove(id)
    {
        stop(report, format!("cannot update the merge state: {error:#}"));
    }
}

fn resume_one(
    env: &Env,
    pending: &Pending,
    client: &mut Client,
    store: &mut Store,
    hooks: &mut Hooks,
    report: &mut StepReport,
) {
    let sha = match &pending.sha {
        Some(sha) => sha.clone(),
        None => {
            let number = pending
                .id
                .rsplit_once('#')
                .and_then(|(_, number)| number.parse::<u64>().ok());
            let Some(number) = number else {
                return fail(report, "the saved id has no PR number");
            };
            match confirm_merge(client, &pending.repo, number) {
                Ok(Some(sha)) => {
                    let _ = store.set_sha(&pending.id, &sha);
                    sha
                }
                Ok(None) => {
                    let _ = store.remove(&pending.id);
                    report.status = Status::Skipped;
                    report.detail = "it was not merged, nothing to wait for".to_string();
                    return;
                }
                Err(error) => {
                    return stop(
                        report,
                        format!("could not tell whether it merged ({error}); run again"),
                    );
                }
            }
        }
    };
    report.merge_sha = Some(sha.clone());
    report.deploy_prediction = pending.deploy_prediction.clone();
    let landed = Landed {
        id: &pending.id,
        slug: &pending.repo,
        base: &pending.base,
        sha: &sha,
    };
    finish_deploy(env, &landed, client, store, hooks, report);
    if report.status == Status::Failed {
        let mut recovered = pending.clone();
        recovered.sha = Some(sha);
        recovery::resolve(env, &recovered, client, store, hooks, report);
    }
}

fn gate_step(env: &Env, hooks: &mut Hooks, report: &mut StepReport) {
    if report.kind == Kind::Wait && package::check(env, hooks, report) {
        return;
    }
    if env.settings.gates_confirmed {
        report.status = Status::Done;
        report.detail = "confirmed with --gates-confirmed".to_string();
    } else if (hooks.gate)(&report.target) {
        report.status = Status::Done;
        report.detail = "confirmed".to_string();
    } else {
        stop(
            report,
            "not confirmed: do it, then run again (in a terminal, or with --gates-confirmed)",
        );
    }
}

struct Bundled {
    slug: String,
    branch: String,
    ids: Vec<String>,
}

fn open_target(slug: &str, branch: &str) -> String {
    format!("{slug}:{branch}")
}

fn arrange(
    report: &mut Report,
    plan: &Plan,
    store: &Store,
    settings: &Settings,
) -> BTreeMap<String, Bundled> {
    report.steps.splice(0..0, resume_steps(&store.state));
    let mut groups: BTreeMap<String, Bundled> = BTreeMap::new();
    if !settings.open_pr {
        return groups;
    }
    let mut last: BTreeMap<String, usize> = BTreeMap::new();
    for (index, step) in report.steps.iter().enumerate() {
        let member = match step.kind {
            Kind::Merge => plan
                .items
                .iter()
                .find(|item| item.id == step.target)
                .map(|item| (format!("{}/{}", item.org, item.repo), item.base.clone())),
            Kind::Resume => store
                .state
                .pending
                .iter()
                .find(|pending| pending.id == step.target)
                .map(|pending| (pending.repo.clone(), pending.base.clone())),
            _ => None,
        };
        let Some((slug, branch)) = member else {
            continue;
        };
        if !is_train_branch(&branch, &plan.deploy_branch) {
            continue;
        }
        let target = open_target(&slug, &branch);
        groups
            .entry(target.clone())
            .or_insert_with(|| Bundled {
                slug,
                branch,
                ids: Vec::new(),
            })
            .ids
            .push(step.target.clone());
        last.insert(target, index);
    }
    let steps = std::mem::take(&mut report.steps);
    for (index, step) in steps.into_iter().enumerate() {
        report.steps.push(step);
        for (target, at) in &last {
            if *at == index {
                report.steps.push(StepReport::new(
                    Kind::OpenPr,
                    target,
                    Status::Planned,
                    "once the last PR of the bundle is merged",
                ));
            }
        }
    }
    for recorded in &store.state.deploy_prs {
        let target = open_target(&recorded.slug, &recorded.branch);
        if let Some(group) = groups.get_mut(&target) {
            for id in &recorded.ids {
                if !group.ids.contains(id) {
                    group.ids.push(id.clone());
                }
            }
            continue;
        }
        report.steps.push(StepReport::new(
            Kind::OpenPr,
            &target,
            Status::Planned,
            "the last PR of the bundle was merged by an earlier run",
        ));
        groups.insert(
            target,
            Bundled {
                slug: recorded.slug.clone(),
                branch: recorded.branch.clone(),
                ids: recorded.ids.clone(),
            },
        );
    }
    groups
}

fn resolve_base(env: &Env, plan: &Plan, bundled: &Bundled, client: &mut Client) -> Result<String> {
    let prefix = format!("{}-", plan.deploy_branch.to_lowercase());
    let lowered = bundled.branch.to_lowercase();
    let Some(suffix) = lowered.strip_prefix(&prefix) else {
        bail!(
            "cannot tell the base of {} from its name: open the PR by hand",
            bundled.branch
        );
    };
    let mut found: Vec<String> = client
        .branch_names(&bundled.slug)?
        .into_iter()
        .filter(|name| !is_stacked(name, &env.cfg.train_prefix) && base_suffix(name) == suffix)
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(anyhow!(
            "no branch of {} matches the base `{suffix}` of {}: open the PR by hand",
            bundled.slug,
            bundled.branch
        )),
        _ => Err(anyhow!(
            "several branches of {} match the base `{suffix}` of {} ({}): open the PR by hand",
            bundled.slug,
            bundled.branch,
            found.join(", ")
        )),
    }
}

fn open_deploy_pr(
    env: &Env,
    plan: &Plan,
    bundled: &Bundled,
    client: &mut Client,
    report: &mut StepReport,
) -> bool {
    let queued = plan.items.iter().find(|item| {
        item.head == bundled.branch
            && format!("{}/{}", item.org, item.repo).eq_ignore_ascii_case(&bundled.slug)
    });
    if let Some(item) = queued {
        report.status = Status::Skipped;
        report.detail = format!("the deploy PR {} is already in the queue", item.id);
        return true;
    }
    match client.branch_tip(&bundled.slug, &bundled.branch) {
        Ok(Some(_)) => {}
        Ok(None) => {
            report.status = Status::Skipped;
            report.detail = format!("the branch {} no longer exists", bundled.branch);
            return true;
        }
        Err(error) => {
            fail(report, format!("{error:#}"));
            return false;
        }
    }
    match client.open_pr_into(&bundled.slug, &bundled.branch) {
        Ok(Some(number)) => {
            report.status = Status::Skipped;
            report.detail = format!(
                "{}#{number} still targets {}: merge or retarget it first",
                bundled.slug, bundled.branch
            );
            return false;
        }
        Ok(None) => {}
        Err(error) => {
            fail(report, format!("{error:#}"));
            return false;
        }
    }
    let base = match resolve_base(env, plan, bundled, client) {
        Ok(base) => base,
        Err(error) => {
            fail(report, format!("{error:#}"));
            return false;
        }
    };
    match client.open_pr_from(&bundled.slug, &bundled.branch, &base) {
        Ok(Some(number)) => {
            report.status = Status::Skipped;
            report.detail = format!("deploy PR already open: {}#{number}", bundled.slug);
            return true;
        }
        Ok(None) => {}
        Err(error) => {
            fail(report, format!("{error:#}"));
            return false;
        }
    }
    let ids: Vec<&str> = bundled.ids.iter().map(String::as_str).collect();
    let body = deploy_pr_body(&base, &ids);
    match client.create_pr(
        &bundled.slug,
        &bundled.branch,
        &bundled.branch,
        &base,
        &body,
    ) {
        Ok(PrWrite::Created) => {
            report.status = Status::Done;
            report.detail = format!("opened {} into {base}", bundled.branch);
            true
        }
        Ok(PrWrite::AlreadyExists) => {
            report.status = Status::Skipped;
            report.detail = "deploy PR already open".to_string();
            true
        }
        Err(error) => {
            fail(report, error.to_string());
            false
        }
    }
}

pub fn merge_plan(
    plan: &Plan,
    cfg: &Config,
    client: &mut Client,
    settings: &Settings,
    store: &mut Store,
    hooks: &mut Hooks,
) -> Result<Outcome> {
    let method = method_of(settings, cfg)?;
    if settings.apply {
        store.lock()?;
    }
    let profile = Some(
        plan.profile
            .as_ref()
            .map_or(crate::profile::IMPLICIT, |profile| profile.name.as_str()),
    );
    let env = Env {
        plan,
        cfg,
        settings,
        method: &method,
        readings: RefCell::new(BTreeMap::new()),
        stacked: stacked::Evidence::default(),
    };
    let mut report = Report {
        max_parallel: cfg.max_parallel,
        summary: Summary::default(),
        preflight: plan
            .preflight
            .iter()
            .filter(|pr| !plan.is_hidden(&pr.id))
            .cloned()
            .collect(),
        state: store.preview(profile, cfg.merge.stale_pending_hours),
        bump_enabled: settings.bump,
        bump_candidates: if settings.bump {
            plan.items
                .iter()
                .filter(|item| !item.hidden && bump::applicable(item, store.state.bump.as_ref()))
                .map(|item| item.id.clone())
                .collect()
        } else {
            Vec::new()
        },
        schema: REPORT_SCHEMA,
        applied: false,
        requested: settings.apply,
        stopped: None,
        steps: planned_steps(plan, cfg),
        not_merging: not_merging(plan),
        questions: emit::question_lines(plan),
    };
    if !settings.apply {
        for step in &mut report.steps {
            if step.kind != Kind::Merge {
                continue;
            }
            if let Some(item) = plan.items.iter().find(|item| item.id == step.target) {
                let key = PrKey::new(&item.org, &item.repo, item.number);
                step.deploy_prediction =
                    Some(env.push_reading(client, &key, &item.base, &item.sha, &item.sha));
            }
        }
        arrange(&mut report, plan, store, settings);
        return Ok(Outcome {
            report,
            aborted: false,
            code: 0,
        });
    }
    if !report.questions.is_empty() && !settings.allow_questions {
        report.stopped =
            Some("the plan has open questions: answer them, or pass --allow-questions".to_string());
        return Ok(Outcome {
            report,
            aborted: false,
            code: emit::EXIT_QUESTIONS,
        });
    }
    let bundles = arrange(&mut report, plan, store, settings);
    if report.steps.is_empty() {
        return Ok(Outcome {
            report,
            aborted: false,
            code: 0,
        });
    }
    if !(hooks.confirm)(&render(&report, false)) {
        return Ok(Outcome {
            report,
            aborted: true,
            code: 1,
        });
    }
    report.applied = true;
    if store.state.profile.is_none() {
        store.state.profile = profile.map(str::to_string);
    }
    if cfg.max_parallel > 1 || settings.bump {
        parallel::execute(&env, client, store, hooks, &mut report.steps, &bundles)?;
        report.summary = Summary::from_steps(&report.steps);
        let code = report.exit_code();
        return Ok(Outcome {
            report,
            aborted: false,
            code,
        });
    }
    let mut progress_state = crate::progress::merge::MergeProgress::new(report.steps.len());
    let mut halted: Option<String> = None;
    let mut failures = failure::DeployFailures::default();
    for index in 0..report.steps.len() {
        let mut step = report.steps[index].clone();
        if let Some(blocker) = &halted {
            step.status = Status::Skipped;
            step.detail = STOPPED.to_string();
            step.blocked_by = Some(blocker.clone());
        } else if let Some(blocked) =
            failures.blocker(index, &report.steps, plan, &bundles, &store.state)
        {
            step.status = Status::Skipped;
            step.detail = blocked.detail;
            step.blocked_by = Some(blocked.id);
        } else {
            let target = progress_target(plan, &store.state, &step);
            progress_state.start(index, target, step.kind);
            let mut progress = |text: &str| {
                if text.starts_with("merged ") {
                    progress_state.merged(index);
                }
                progress_state.update(index, text);
                (hooks.progress)(&progress_state.frame());
            };
            let mut step_hooks = Hooks {
                confirm: &mut *hooks.confirm,
                gate: &mut *hooks.gate,
                sleep: &mut *hooks.sleep,
                observe: &mut *hooks.observe,
                progress: &mut progress,
            };
            (step_hooks.progress)("starting");
            match step.kind {
                Kind::Resume => {
                    let pending = store
                        .state
                        .pending
                        .iter()
                        .find(|pending| pending.id == step.target)
                        .cloned();
                    match pending {
                        Some(pending) => {
                            resume_one(&env, &pending, client, store, &mut step_hooks, &mut step);
                        }
                        None => fail(&mut step, "the saved state no longer lists it"),
                    }
                }
                Kind::Merge => match plan.items.iter().find(|item| item.id == step.target) {
                    Some(item) => merge_one(&env, item, client, store, &mut step_hooks, &mut step),
                    None => fail(&mut step, "it is not in the plan"),
                },
                Kind::Bump => fail(&mut step, "bump events are managed by the coordinator"),
                Kind::Gate | Kind::Wait => gate_step(&env, &mut step_hooks, &mut step),
                Kind::OpenPr => match bundles.get(&step.target) {
                    Some(bundled) => {
                        let settled = open_deploy_pr(&env, plan, bundled, client, &mut step);
                        if settled
                            && let Err(error) =
                                store.remove_deploy_pr(&bundled.slug, &bundled.branch)
                        {
                            stop(
                                &mut step,
                                format!("cannot update the merge state: {error:#}"),
                            );
                        }
                    }
                    None => fail(&mut step, "the bundle is not in this run"),
                },
            }
            if step.status == Status::Failed
                && step.merge_sha.is_some()
                && matches!(step.kind, Kind::Merge | Kind::Resume)
            {
                let pending = store
                    .state
                    .pending
                    .iter()
                    .find(|pending| pending.id == step.target);
                match pending {
                    Some(pending) if pending.continuation.is_some() => {
                        let has_consumers = failures.record(pending, plan);
                        if has_consumers && settings.dependency_failure == DependencyFailure::Stop {
                            step.detail.push_str(
                                "; dependent PRs require this deploy (--dependency-failure stop)",
                            );
                            halted = Some(step.target.clone());
                        }
                    }
                    _ => {
                        step.detail.push_str(
                            "; the saved state has no independence proof, stopping safely",
                        );
                        halted = Some(step.target.clone());
                    }
                }
            } else if matches!(step.status, Status::Failed | Status::Stopped) {
                halted = Some(step.target.clone());
            }
        }
        progress_state.finish(index, &step);
        (hooks.observe)(&step);
        (hooks.progress)(&progress_state.frame());
        report.steps[index] = step;
    }
    report.summary = Summary::from_steps(&report.steps);
    let code = report.exit_code();
    Ok(Outcome {
        report,
        aborted: false,
        code,
    })
}

fn step_text(step: &StepReport, color: bool) -> String {
    let what = match step.kind {
        Kind::Resume => format!("wait for the deploy of {}", step.target),
        Kind::Bump => format!("prepare dependency bump for {}", step.target),
        Kind::Merge => format!("merge {}", step.target),
        Kind::Gate => format!("gate: {}", step.target),
        Kind::Wait => format!("wait: {}", step.target),
        Kind::OpenPr => format!("open the deploy PR {}", step.target),
    };
    let what = if step.kind == Kind::Merge {
        tint(color, bold, &what)
    } else {
        what
    };
    let suffix = if step.detail.is_empty() {
        String::new()
    } else {
        format!(" ({})", step.detail)
    };
    let deploy = step
        .deploy
        .as_ref()
        .map(|text| format!("; {text}"))
        .unwrap_or_default();
    match step.status {
        Status::Planned => {
            let prediction = step
                .deploy_prediction
                .as_ref()
                .map(|reading| {
                    let branch = step
                        .detail
                        .strip_prefix("into ")
                        .and_then(|detail| detail.split(';').next())
                        .unwrap_or("the saved base");
                    format!("; {}", reading.detail(branch))
                })
                .unwrap_or_default();
            format!("{what}{suffix}{deploy}{prediction}")
        }
        Status::Done => format!("{} {what}{suffix}{deploy}", tint(color, green, "done:")),
        Status::Skipped => format!(
            "{} {what}{suffix}{deploy}",
            tint(
                color,
                if step.blocked_by.is_some() {
                    yellow
                } else {
                    dim
                },
                "skipped:"
            )
        ),
        Status::Failed => format!("{} {what}{suffix}{deploy}", tint(color, red, "failed:")),
        Status::Stopped => format!("{} {what}{suffix}{deploy}", tint(color, yellow, "stopped:")),
    }
}

pub fn render(report: &Report, color: bool) -> String {
    render_with_steps(report, color, true)
}

pub fn render_final(report: &Report, color: bool) -> String {
    render_with_steps(report, color, !report.applied)
}

fn render_with_steps(report: &Report, color: bool, include_steps: bool) -> String {
    let mut out = String::new();
    if !report.applied && !report.requested && report.stopped.is_none() {
        let banner = "dry run, nothing was merged; pass --apply to do it";
        out.push_str(&format!("{}\n\n", tint(color, yellow, banner)));
    }
    if include_steps {
        out.push_str(&format!(
            "up to {} PRs at a time, one per repository (including different bases)\n",
            report.max_parallel
        ));
        out.push_str(&report.state.render());
        for pr in &report.preflight {
            for line in pr.lines() {
                out.push_str(&format!("{line}\n"));
            }
        }
    }
    if report.bump_enabled && include_steps {
        out.push_str("--bump enabled: with --apply this run may create isolated bump branches and draft PRs targeting consumer heads, then block those consumers for CI and review. Dry runs perform no writes.\n");
        for target in &report.bump_candidates {
            out.push_str(&format!("potential dependency bump: {target} (only after a new publication is verified; no version chosen yet)\n"));
        }
        out.push('\n');
    }
    if report.steps.is_empty() {
        out.push_str(&format!("{}\n", tint(color, dim, "nothing to merge")));
    }
    if include_steps {
        for step in &report.steps {
            out.push_str(&format!("{}\n", step_text(step, color)));
        }
    }
    if report
        .steps
        .iter()
        .any(|step| step.detail.contains(TRAIN_NOTICE))
    {
        out.push_str(&format!(
            "\n{}\n",
            tint(color, yellow, "warning: this queue includes intermediate train branches. Their names are not today's date or proof of deployment. Promotion requires a separate reviewed PR; --open-pr only attempts to prepare it and never merges it.")
        ));
    }
    if !report.not_merging.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            tint(
                color,
                dim,
                "not merging, the plan marks these as not ready:"
            )
        ));
        for skipped in &report.not_merging {
            out.push_str(&format!(
                "  {}: {}\n",
                skipped.id,
                skipped.reasons.join(", ")
            ));
        }
    }
    for question in &report.questions {
        out.push_str(&format!("{question}\n"));
    }
    if let Some(reason) = &report.stopped {
        out.push_str(&format!("\n{}\n", tint(color, yellow, reason)));
    }
    if report.applied {
        let summary = &report.summary;
        out.push_str(&format!(
            "\n{} PRs merged, {} earlier merges checked\n",
            summary.merged, summary.earlier_merges_checked,
        ));
        out.push_str(&format!(
            "deploys: {} confirmed, {} dispensed, {} indeterminate, {} resolved by later deploy, {} skipped, {} failed\n",
            summary.deploy_confirmed, summary.deploy_dispensed, summary.deploy_indeterminate,
            summary.deploy_resolved, summary.deploy_skipped, summary.deploy_failed,
        ));
        out.push_str(&format!(
            "\n{} done, {} skipped, {} failed, {} stopped\n",
            report.count(Status::Done),
            report.count(Status::Skipped),
            report.count(Status::Failed),
            report.count(Status::Stopped)
        ));
        if report.steps.iter().any(|step| {
            step.merge_sha.is_some() && matches!(step.status, Status::Failed | Status::Stopped)
        }) {
            out.push_str("The PR is already merged and its deploy is still pending in the state file. Inspect it with pipetrain state show. Repeat the command with the same --state to resume. Resume checks the saved SHA and run, then a later deploy containing that SHA if the saved deploy failed. Repeating without a changed run or a later successful deploy will not fix a failed deploy. An intentional local discard requires pipetrain state discard with a reason. The merge will not be repeated.\n");
        }
    }
    out
}

fn ask(question: &str, prompt: &str) -> String {
    eprintln!("{question}");
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line
}

pub fn run(opts: MergeOptions) -> Result<i32> {
    let interactive = std::io::stdin().is_terminal();
    if gate(opts.apply, opts.yes, interactive, None) == Gate::Refuse {
        bail!("refusing to merge without a terminal: pass --yes");
    }
    let started = Instant::now();
    let json = opts.plan.json;
    let (built, mut client, cfg) = build_plan_with_config(&opts.plan)?;
    let state_path = resolve_state_path(
        opts.state.as_deref(),
        cfg.merge.state_file.as_deref(),
        built.profile.as_ref().map(|profile| profile.name.as_str()),
    )?;
    let mut store = Store::open(Some(state_path.clone()))?;
    if !json && opts.plan.progress {
        eprintln!("state: {}", state_path.display());
    }
    let settings = Settings {
        apply: opts.apply,
        gates_confirmed: opts.gates_confirmed,
        allow_questions: opts.allow_questions,
        method: opts.method.clone(),
        timeout: opts
            .timeout_minutes
            .map(|minutes| Duration::from_secs(minutes.saturating_mul(60))),
        feed_registry: None,
        open_pr: opts.open_pr,
        dependency_failure: opts.dependency_failure,
        bump: opts.bump,
        bump_options: opts.bump_options.clone(),
    };
    let (apply, yes) = (opts.apply, opts.yes);
    let live = crate::progress::detect(!opts.plan.progress, json, false);
    let display = RefCell::new(None);
    let execution_started = RefCell::new(None);
    let mut confirm = |text: &str| {
        let answer = if yes {
            None
        } else {
            Some(ask(
                text,
                if opts.bump {
                    "Merge these PRs and prepare draft dependency bump PRs where publication is proved, blocking consumers for CI and review? Type yes: "
                } else {
                    "Merge these PRs with the configured parallelism, waiting for each deploy? Type yes: "
                },
            ))
        };
        let proceed = gate(apply, yes, interactive, answer.as_deref()) == Gate::Proceed;
        if proceed {
            let started = *execution_started
                .borrow_mut()
                .get_or_insert_with(Instant::now);
            *display.borrow_mut() = Some(crate::progress::Progress::start_at(live, started));
        }
        proceed
    };
    let mut at_gate = |text: &str| {
        display.borrow_mut().take();
        if !interactive {
            return false;
        }
        let answer = ask(
            &format!("Gate: {text}"),
            "Type yes once that is done, anything else stops here: ",
        );
        let proceed = answer.trim() == "yes";
        if proceed {
            let started = *execution_started
                .borrow_mut()
                .get_or_insert_with(Instant::now);
            *display.borrow_mut() = Some(crate::progress::Progress::start_at(live, started));
        }
        proceed
    };
    let mut pause = |wait: Duration| std::thread::sleep(wait);
    let mut observe = |step: &StepReport| {
        if !json {
            crate::progress::line(&step_text(step, true));
        }
    };
    let mut progress = |text: &str| crate::progress::phase(text);
    let mut hooks = Hooks {
        confirm: &mut confirm,
        gate: &mut at_gate,
        sleep: &mut pause,
        observe: &mut observe,
        progress: &mut progress,
    };
    let mut outcome = merge_plan(&built, &cfg, &mut client, &settings, &mut store, &mut hooks)?;
    if opts.plan.strict
        && outcome.code == 0
        && (!built.has_eligible()
            || (!opts.apply
                && !outcome
                    .report
                    .steps
                    .iter()
                    .any(|step| step.kind == Kind::Merge)))
    {
        outcome.code = emit::EXIT_NO_ELIGIBLE;
    }
    display.borrow_mut().take();
    if outcome.aborted {
        eprintln!("aborted, nothing was merged");
        return Ok(outcome.code);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome.report)?);
    } else {
        anstream::print!("{}", render_final(&outcome.report, true));
    }
    if opts.apply {
        eprintln!(
            "{}",
            requests_line(client.requests(), client.cache_hits(), started.elapsed())
        );
        if outcome.code == crate::merge::EXIT_STOPPED {
            eprintln!("stopped, nothing is lost: run the same command again to resume");
        }
    }
    Ok(outcome.code)
}
