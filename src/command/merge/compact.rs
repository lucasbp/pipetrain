use std::collections::BTreeMap;

use serde_json::Value;

use super::{KEPT, is_kept};
use crate::command::apply::tint;
use crate::config::GateKind;
use crate::github::Client;
use crate::merge::category::Category;
use crate::merge::{DeployOutcome, Kind, Report, Status, StepReport};
use crate::plan::Plan;
use crate::style::{bold, cyan, dim, green, red, yellow};

const CAUSE_WIDTH: usize = 110;

#[derive(Debug, Default, Clone)]
pub struct Context {
    pub command: String,
    pub state: Option<String>,
    pub details: BTreeMap<String, String>,
}

pub fn short(id: &str) -> String {
    match id.split_once('/') {
        Some((_, rest)) if rest.contains('#') => rest.to_string(),
        _ => id.to_string(),
    }
}

pub fn shorten(text: &str) -> String {
    text.split(' ')
        .map(|token| {
            let start = token.len() - token.trim_start_matches('(').len();
            let body = token[start..].trim_end_matches([',', ';', ':', ')', '.']);
            let tail = &token[start + body.len()..];
            match body.split_once('/') {
                Some((_, rest)) if body.contains('#') && !body.contains("://") => {
                    format!("{}{rest}{tail}", &token[..start])
                }
                _ => token.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

pub fn plain_cause(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower == "review required" || lower == "not approved" {
        return "needs human approval".to_string();
    }
    if lower == "changes requested" {
        return "changes requested, address them first".to_string();
    }
    if lower == "draft" || lower.starts_with("draft (") {
        return "draft, mark it ready for review".to_string();
    }
    if lower == "checks pending" {
        return "checks still running".to_string();
    }
    if let Some(names) = reason.strip_prefix("checks failing: ") {
        let names = names.trim();
        return if names.contains(',') {
            format!("checks {names} failing")
        } else {
            format!("check {names} failing")
        };
    }
    if lower == "checks failing" || lower.starts_with("checks are not passing") {
        return "checks failing".to_string();
    }
    if lower == "no checks reported" {
        return "no checks configured in the repo".to_string();
    }
    if lower.starts_with("merge conflict") {
        return "conflict with base".to_string();
    }
    if lower.starts_with("branch behind") {
        return "branch behind its base, update it".to_string();
    }
    if lower.starts_with("branch protection blocks merge") {
        return "blocked by branch protection (usually a missing approval or check)".to_string();
    }
    if lower.starts_with("already merged") || lower == "closed" {
        return "already merged or closed".to_string();
    }
    if let Some(rest) = reason.strip_prefix("waiting for package ") {
        return format!("waiting for package {}", shorten(rest));
    }
    if let Some(text) = release_behind(reason) {
        return text;
    }
    if let Some((_, rest)) = reason.split_once(" depends on ") {
        let target = rest.split([',', ';']).next().unwrap_or(rest).trim();
        return format!("depends on {} first", shorten(target));
    }
    shorten(reason)
}

fn number_after(text: &str, marker: &str) -> Option<String> {
    let rest = text.split_once(marker)?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then_some(digits)
}

fn release_behind(reason: &str) -> Option<String> {
    let rest = reason.strip_prefix("references ")?;
    let package = rest.split_whitespace().next()?;
    let count = number_after(reason, "the queue publishes ")?;
    let limit = number_after(reason, "the repo check allows ")?;
    let noun = if count == "1" { "version" } else { "versions" };
    Some(format!(
        "queue publishes {count} {noun} of {package}, repo accepts at most {limit} behind"
    ))
}

fn wait_cause(text: &str) -> String {
    match text.strip_prefix("package ") {
        Some(rest) => {
            let name = rest.split(" from ").next().unwrap_or(rest);
            if text.trim_end().ends_with("(not yet)") {
                format!("waiting for package {name}")
            } else {
                format!("package {name} published")
            }
        }
        None => truncate(&shorten(text), CAUSE_WIDTH),
    }
}

fn cell(color: bool, style: fn(&str) -> String, text: &str, width: usize) -> String {
    let padded = format!("{text:<width$}");
    tint(color, style, &padded)
}

fn ordered_after(plan: &Plan, target: &str) -> Vec<String> {
    let Some(item) = plan.items.iter().find(|item| item.id == target) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = Vec::new();
    for dependency in &item.after {
        let id = short(&dependency.id);
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn gate_summary(plan: &Plan) -> Option<String> {
    let ddl = plan
        .items
        .iter()
        .filter(|item| !item.hidden && matches!(item.gate, Some(GateKind::Ddl)))
        .count();
    let infra = plan
        .items
        .iter()
        .filter(|item| !item.hidden && matches!(item.gate, Some(GateKind::Infra)))
        .count();
    let mut parts = Vec::new();
    if ddl > 0 {
        parts.push(format!("{ddl} DDL"));
    }
    if infra > 0 {
        parts.push(format!("{infra} infra"));
    }
    (!parts.is_empty()).then(|| format!("gates: {}", parts.join(", ")))
}

pub fn footer_counts(report: &Report, color: bool) -> String {
    let waiting = report.count_state(Category::Waiting);
    let blocked = report.count_state(Category::Blocked);
    format!(
        "left out of the queue: {} · {} (never changes the exit code)",
        tint(
            color,
            if waiting == 0 { dim } else { yellow },
            &format!("{waiting} waiting")
        ),
        tint(
            color,
            if blocked == 0 { dim } else { red },
            &format!("{blocked} blocked")
        ),
    )
}

pub fn preview(report: &Report, plan: &Plan, color: bool) -> String {
    let mut out = String::new();
    if !report.applied && !report.requested && report.stopped.is_none() {
        let banner = "dry run, nothing was merged; pass --apply to do it";
        out.push_str(&format!("{}\n\n", tint(color, yellow, banner)));
    }
    let name_of = |step: &StepReport| match step.kind {
        Kind::Merge | Kind::Resume => short(&step.target),
        Kind::Gate => "gate".to_string(),
        Kind::Wait => "wait".to_string(),
        Kind::Bump => format!("bump {}", short(&step.target)),
        Kind::OpenPr => "deploy PR".to_string(),
    };
    let width = report
        .steps
        .iter()
        .map(|step| name_of(step).chars().count())
        .chain(
            report
                .not_merging
                .iter()
                .map(|skipped| short(&skipped.id).chars().count()),
        )
        .max()
        .unwrap_or(0);
    for (index, step) in report.steps.iter().enumerate() {
        let number = format!("{:>2}", index + 1);
        let name = name_of(step);
        let (state, state_style, note): (&str, fn(&str) -> String, String) = match step.kind {
            Kind::Merge => {
                let after = ordered_after(plan, &step.target);
                let mut note = if after.is_empty() {
                    String::new()
                } else {
                    format!("after {}", after.join(", "))
                };
                if let Some(seconds) = step.settle_seconds {
                    if !note.is_empty() {
                        note.push_str("; ");
                    }
                    note.push_str(&format!("settling {seconds}s"));
                }
                if step.admin_override.is_some() {
                    if !note.is_empty() {
                        note.push_str("; ");
                    }
                    note.push_str("merges past a missing review (--admin)");
                }
                let no_checks = plan.preflight.iter().any(|readiness| {
                    readiness.id == step.target && !readiness.exceptions.is_empty()
                });
                if no_checks {
                    if !note.is_empty() {
                        note.push_str("; ");
                    }
                    note.push_str("no checks reported (allowed by config)");
                }
                ("ready", green, note)
            }
            Kind::Resume => (
                "resume",
                cyan,
                if step.detail.starts_with(KEPT) {
                    "kept, failed deploy from an earlier run".to_string()
                } else {
                    "waits for the deploy of an earlier merge".to_string()
                },
            ),
            Kind::Gate => (
                "pause",
                yellow,
                truncate(&shorten(&step.target), CAUSE_WIDTH),
            ),
            Kind::Wait => ("wait", yellow, wait_cause(&step.target)),
            Kind::Bump => (
                "bump",
                cyan,
                if step.detail.starts_with("bump ") {
                    truncate(&shorten(&step.detail), CAUSE_WIDTH)
                } else {
                    "prepares a dependency bump".to_string()
                },
            ),
            Kind::OpenPr => ("open-pr", cyan, "opens the deploy PR".to_string()),
        };
        let name_cell = match step.kind {
            Kind::Merge | Kind::Resume => cell(color, cyan, &name, width),
            _ => format!("{name:<width$}"),
        };
        let (state_cell, note) = if note.is_empty() {
            (tint(color, state_style, state), String::new())
        } else {
            (
                cell(color, state_style, state, 7),
                format!("  {}", tint(color, dim, &note)),
            )
        };
        out.push_str(&format!("{number}  {name_cell}  {state_cell}{note}\n"));
    }
    for skipped in &report.not_merging {
        let cause = skipped
            .reasons
            .first()
            .map(|reason| plain_cause(reason))
            .unwrap_or_else(|| "not ready".to_string());
        let more = skipped.reasons.len().saturating_sub(1);
        let tail = if more > 0 {
            tint(color, dim, &format!(" (+{more} more, see --verbose)"))
        } else {
            String::new()
        };
        let (label, label_style): (&str, fn(&str) -> String) = match skipped.state {
            Category::Waiting => ("waiting", yellow),
            Category::Blocked => ("blocked", red),
        };
        out.push_str(&format!(
            "{:>2}  {}  {}  {}{}\n",
            "-",
            cell(color, cyan, &short(&skipped.id), width),
            cell(color, label_style, label, 7),
            tint(color, bold, &truncate(&cause, CAUSE_WIDTH)),
            tail,
        ));
    }
    if report.steps.is_empty() && report.not_merging.is_empty() {
        out.push_str(&format!("{}\n", tint(color, dim, "nothing to merge")));
    }
    out.push('\n');
    if report.requested && !report.applied {
        let notice = super::admin::notice(&report.steps);
        if !notice.is_empty() {
            out.push_str(&tint(color, yellow, notice.trim_end()));
            out.push_str("\n\n");
        }
    }
    let entering = report
        .steps
        .iter()
        .filter(|step| step.kind == Kind::Merge)
        .count();
    let waiting = report.count_state(Category::Waiting);
    let blocked = report.count_state(Category::Blocked);
    let mut parts = vec![
        tint(color, bold, &format!("{entering} enter the queue")),
        tint(
            color,
            if waiting == 0 { dim } else { yellow },
            &format!("{waiting} waiting"),
        ),
        tint(
            color,
            if blocked == 0 { dim } else { red },
            &format!("{blocked} blocked"),
        ),
    ];
    if let Some(gates) = gate_summary(plan) {
        parts.push(tint(color, yellow, &gates));
    }
    if !report.questions.is_empty() {
        parts.push(tint(
            color,
            yellow,
            &format!("{} open questions", report.questions.len()),
        ));
    }
    out.push_str(&format!("{}\n", parts.join(" · ")));
    for question in &report.questions {
        out.push_str(&format!(
            "  {}\n",
            tint(color, dim, &truncate(&shorten(question), CAUSE_WIDTH))
        ));
    }
    let estimate = &report.api_estimate;
    if let Some(warning) = estimate.warning() {
        out.push_str(&format!("{}\n", tint(color, yellow, &warning)));
    }
    if !report.state.pending.is_empty() {
        let ids: Vec<String> = report
            .state
            .pending
            .iter()
            .map(|pending| short(&pending.id))
            .collect();
        let stale = report
            .state
            .pending
            .iter()
            .filter(|pending| pending.stale)
            .count();
        let stale = if stale > 0 {
            format!(", {stale} stale")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{}\n",
            tint(
                color,
                dim,
                &format!(
                    "inherited state: {} pending from earlier runs ({}{stale})",
                    ids.len(),
                    ids.join(", ")
                )
            )
        ));
    }
    if report
        .steps
        .iter()
        .any(|step| step.detail.contains(super::TRAIN_NOTICE))
    {
        out.push_str(&format!(
            "{}\n",
            tint(
                color,
                yellow,
                "warning: the queue includes intermediate train branches, not proof of deployment"
            )
        ));
    }
    if let Some(reason) = &report.stopped {
        out.push_str(&format!("{}\n", tint(color, yellow, reason)));
    }
    out.push_str(&format!(
        "{}\n",
        tint(
            color,
            dim,
            "--verbose shows checks, URLs and the full detail"
        )
    ));
    out
}

fn deploy_ok(step: &StepReport) -> bool {
    step.resolution.is_some()
        || matches!(
            step.deploy_outcome,
            Some(DeployOutcome::Confirmed | DeployOutcome::Dispensed | DeployOutcome::Skipped)
        )
}

fn merged_with_failed_deploy(step: &StepReport) -> bool {
    step.merge_sha.is_some() && step.status == Status::Failed
}

fn first_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
        .map(|token| token.trim_end_matches([',', ';', ')', '.']).to_string())
}

fn deploy_sentence(step: &StepReport) -> String {
    let text = step.deploy.as_deref().unwrap_or(&step.detail);
    let without_url = match first_url(text) {
        Some(url) => text
            .replace(&url, "")
            .trim_end()
            .trim_end_matches(':')
            .trim()
            .to_string(),
        None => text.trim().to_string(),
    };
    truncate(&shorten(&without_url), 160)
}

fn failed_run_url(step: &StepReport) -> Option<String> {
    step.deploy
        .as_deref()
        .and_then(first_url)
        .or_else(|| first_url(&step.detail))
        .or_else(|| {
            step.expected_runs
                .iter()
                .find(|run| {
                    matches!(
                        run.conclusion.as_deref(),
                        Some("failure" | "timed_out" | "cancelled" | "startup_failure")
                    )
                })
                .map(|run| run.html_url.clone())
        })
}

pub fn finished_line(step: &StepReport, color: bool) -> String {
    let label = match step.kind {
        Kind::Merge | Kind::Resume => cell(color, cyan, &short(&step.target), 0),
        Kind::Gate => format!("gate {}", truncate(&shorten(&step.target), 60)),
        Kind::Wait => format!("wait {}", wait_cause(&step.target)),
        Kind::Bump => format!("bump {}", short(&step.target)),
        Kind::OpenPr => format!("deploy PR {}", short(&step.target)),
    };
    let cause = |text: &str| truncate(&shorten(text), CAUSE_WIDTH);
    match step.status {
        Status::Planned => format!("{label}  planned"),
        Status::Done if matches!(step.kind, Kind::Merge | Kind::Resume) => {
            if deploy_ok(step) {
                format!("{label}  {}", tint(color, green, "merged + deploy ok"))
            } else if step.merge_sha.is_some() {
                format!(
                    "{label}  {}",
                    tint(color, yellow, "merged, deploy not confirmed")
                )
            } else {
                format!("{label}  {}", tint(color, green, "done"))
            }
        }
        Status::Done => format!("{label}  {}", tint(color, green, "done")),
        Status::Failed if merged_with_failed_deploy(step) => format!(
            "{label}  {}  {}",
            tint(color, red, "merged, deploy failed"),
            tint(color, dim, &deploy_sentence(step))
        ),
        Status::Failed => format!(
            "{label}  {}  {}",
            tint(color, red, "failed"),
            tint(color, dim, &cause(&step.detail))
        ),
        Status::Stopped => format!(
            "{label}  {}  {}",
            tint(color, yellow, "stopped"),
            tint(color, dim, &cause(&step.detail))
        ),
        Status::Skipped if step.kind == Kind::Bump => format!(
            "{label}  {}  {}",
            tint(color, yellow, "waiting"),
            tint(color, dim, &cause(&step.detail))
        ),
        Status::Skipped if step.blocked_by.is_some() => format!(
            "{label}  {}  {}",
            tint(color, yellow, "stopped"),
            tint(
                color,
                dim,
                &cause(step.blocked_by.as_deref().unwrap_or_default())
            )
        ),
        Status::Skipped => format!(
            "{label}  {}  {}",
            tint(color, dim, "skipped"),
            tint(color, dim, &cause(&step.detail))
        ),
    }
}

fn quote(argument: &str) -> String {
    if argument.is_empty() || argument.contains([' ', '\t', '"', '\'']) {
        format!("\"{}\"", argument.replace('"', "\\\""))
    } else {
        argument.to_string()
    }
}

pub fn command_line(arguments: &[String], state: Option<&str>) -> String {
    let mut parts: Vec<String> = arguments.iter().map(|argument| quote(argument)).collect();
    let has_state = arguments
        .iter()
        .any(|argument| argument == "--state" || argument.starts_with("--state="));
    if !has_state && let Some(state) = state {
        parts.push("--state".to_string());
        parts.push(quote(state));
    }
    format!("pipetrain {}", parts.join(" "))
}

fn discard_line(id: &str, state: Option<&str>) -> String {
    let mut line = format!("pipetrain state discard {id} --reason \"<why>\"");
    if let Some(state) = state {
        line.push_str(&format!(" --state {}", quote(state)));
    }
    line
}

pub fn final_report(report: &Report, color: bool, context: &Context) -> String {
    let mut out = String::new();
    if let Some(reason) = &report.stopped {
        out.push_str(&format!("{}\n\n", tint(color, yellow, reason)));
    }
    if report.steps.is_empty() {
        out.push_str(&format!("{}\n", tint(color, dim, "nothing to merge")));
        if !report.not_merging.is_empty() {
            out.push_str(&format!("{}\n", footer_counts(report, color)));
        }
        return out;
    }
    let summary = &report.summary;
    let deploys_ok = summary.deploy_confirmed + summary.deploy_dispensed + summary.deploy_resolved;
    let failed = report.count(Status::Failed);
    let stopped = report.count(Status::Stopped)
        + report
            .steps
            .iter()
            .filter(|step| step.status == Status::Skipped && step.blocked_by.is_some())
            .count();
    let counts = format!(
        "{} merged · {} deploys ok · {} failed · {} stopped",
        summary.merged, deploys_ok, failed, stopped
    );
    out.push_str(&format!("{}\n", tint(color, bold, &counts)));
    if !report.not_merging.is_empty() {
        out.push_str(&format!("{}\n", footer_counts(report, color)));
    }
    let ok: Vec<String> = report
        .steps
        .iter()
        .filter(|step| {
            step.status == Status::Done
                && matches!(step.kind, Kind::Merge | Kind::Resume)
                && step.merge_sha.is_some()
                && deploy_ok(step)
        })
        .map(|step| short(&step.target))
        .collect();
    if !ok.is_empty() {
        out.push_str(&format!(
            "{} {}\n",
            tint(color, green, "merged + deploy ok:"),
            ok.join(", ")
        ));
    }
    let unconfirmed: Vec<String> = report
        .steps
        .iter()
        .filter(|step| {
            step.status == Status::Done
                && matches!(step.kind, Kind::Merge | Kind::Resume)
                && step.merge_sha.is_some()
                && !deploy_ok(step)
        })
        .map(|step| short(&step.target))
        .collect();
    if !unconfirmed.is_empty() {
        out.push_str(&format!(
            "{} {}\n",
            tint(color, yellow, "merged, deploy not confirmed:"),
            unconfirmed.join(", ")
        ));
    }
    for step in &report.steps {
        if step.merge_sha.is_some()
            && let Some(admin) = &step.admin_override
            && admin.at.is_some()
        {
            out.push_str(&format!(
                "{}\n",
                tint(
                    color,
                    yellow,
                    &format!(
                        "merged by admin override: {} ({})",
                        step.target, admin.reason
                    )
                )
            ));
        }
    }
    for step in report.steps.iter().filter(|step| is_kept(step)) {
        out.push_str(&format!(
            "{}\n",
            tint(
                color,
                dim,
                &format!(
                    "kept, failed deploy from an earlier run: {}",
                    short(&step.target)
                )
            )
        ));
    }
    let state = context.state.as_deref();
    for step in report.steps.iter().filter(|step| {
        step.status == Status::Failed && matches!(step.kind, Kind::Merge | Kind::Resume)
    }) {
        out.push('\n');
        out.push_str(&failure_block(step, color, context, state));
    }
    for step in report.steps.iter().filter(|step| {
        step.status == Status::Failed && !matches!(step.kind, Kind::Merge | Kind::Resume)
    }) {
        out.push('\n');
        out.push_str(&format!(
            "{}  {}\n  what:    {}\n  next:    fix it, then run again: {}\n",
            tint(color, red, &short(&step.target)),
            tint(color, red, "failed"),
            truncate(&shorten(&step.detail), 160),
            context.command
        ));
    }
    let stopped_steps: Vec<&StepReport> = report
        .steps
        .iter()
        .filter(|step| {
            step.status == Status::Stopped
                || (step.status == Status::Skipped && step.blocked_by.is_some())
        })
        .collect();
    if !stopped_steps.is_empty() {
        out.push('\n');
        let names: Vec<String> = stopped_steps
            .iter()
            .map(|step| match step.kind {
                Kind::Merge | Kind::Resume => short(&step.target),
                Kind::Gate => "gate".to_string(),
                Kind::Wait => "wait".to_string(),
                Kind::Bump => format!("bump {}", short(&step.target)),
                Kind::OpenPr => "deploy PR".to_string(),
            })
            .collect();
        out.push_str(&format!(
            "{} {}\n",
            tint(color, yellow, "stopped, not merged:"),
            names.join(", ")
        ));
        if let Some(first) = stopped_steps.first() {
            let reason = first
                .blocked_by
                .as_deref()
                .filter(|text| !text.is_empty())
                .unwrap_or(&first.detail);
            out.push_str(&format!(
                "  why:     {}\n  next:    fix the cause above, then run again: {}\n",
                truncate(&shorten(reason), 160),
                context.command
            ));
        }
    }
    if report.steps.iter().any(|step| {
        step.merge_sha.is_some() && matches!(step.status, Status::Failed | Status::Stopped)
    }) {
        out.push_str(&format!(
            "\n{}\n",
            tint(
                color,
                dim,
                "tip: pipetrain state show lists pending deploys; the merge is never repeated on resume."
            )
        ));
    }
    out
}

const ACTIVE_DEPLOYMENT: &str = "already has an active deployment";

fn previous_deployment_active(step: &StepReport, context: &Context) -> bool {
    [
        step.deploy.as_deref(),
        Some(step.detail.as_str()),
        context.details.get(&step.target).map(String::as_str),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(ACTIVE_DEPLOYMENT))
}

fn job_label(detail: &str) -> &str {
    detail.split_once("; ").map_or(detail, |(label, _)| label)
}

fn failure_block(step: &StepReport, color: bool, context: &Context, state: Option<&str>) -> String {
    let mut out = String::new();
    let head = if merged_with_failed_deploy(step) {
        "merged, deploy failed"
    } else {
        "failed, not merged"
    };
    out.push_str(&format!(
        "{}  {}\n",
        tint(color, red, &short(&step.target)),
        tint(color, red, head)
    ));
    if merged_with_failed_deploy(step) {
        let sha = step
            .merge_sha
            .as_deref()
            .map(|sha| sha.chars().take(7).collect::<String>())
            .unwrap_or_default();
        let active = previous_deployment_active(step, context);
        let mut what = if active {
            format!(
                "the merge went in at {sha}, but the previous deployment on this target was still active, so the new deploy was refused"
            )
        } else {
            format!("the merge went in at {sha}; {}", deploy_sentence(step))
        };
        if let Some(detail) = context.details.get(&step.target) {
            what.push_str(&format!(" ({})", job_label(detail)));
        }
        out.push_str(&format!("  what:    {what}\n"));
        if let Some(url) = failed_run_url(step) {
            out.push_str(&format!("  run:     {url}\n"));
        }
        if active {
            out.push_str(&format!(
                "  next:    wait until the previous deployment finishes, then resume: {}\n           to avoid it next time, set merge.same_target_settle_seconds in the config\n           or drop it for good: {}\n",
                context.command,
                discard_line(&step.target, state)
            ));
        } else {
            out.push_str("  unknown: whether production changed; open the run before deciding\n");
            out.push_str(&format!(
                "  next:    {}\n           or drop it for good: {}\n",
                context.command,
                discard_line(&step.target, state)
            ));
        }
    } else {
        out.push_str(&format!(
            "  what:    {}\n  next:    fix it, then run again: {}\n",
            truncate(&shorten(&step.detail), 160),
            context.command
        ));
    }
    out
}

pub fn parse_run(url: &str) -> Option<(String, u64)> {
    let (head, tail) = url.split_once("/actions/runs/")?;
    let id: u64 = tail.split(['/', '?', '#']).next()?.parse().ok()?;
    let mut segments = head.trim_end_matches('/').rsplit('/');
    let repo = segments.next()?;
    let org = segments.next()?;
    Some((format!("{org}/{repo}"), id))
}

pub fn describe_jobs(value: &Value) -> Option<String> {
    let jobs = value.get("jobs")?.as_array()?;
    let failed = jobs.iter().find(|job| {
        matches!(
            job.get("conclusion").and_then(Value::as_str),
            Some("failure" | "timed_out" | "cancelled")
        )
    })?;
    let job = failed.get("name")?.as_str()?;
    let step = failed
        .get("steps")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps.iter().find(|step| {
                matches!(
                    step.get("conclusion").and_then(Value::as_str),
                    Some("failure" | "timed_out" | "cancelled")
                )
            })
        })
        .and_then(|step| step.get("name").and_then(Value::as_str));
    Some(match step {
        Some(step) => format!("job {job}, step {step}"),
        None => format!("job {job}"),
    })
}

pub fn enrich(client: &mut Client, report: &Report) -> BTreeMap<String, String> {
    let mut details = BTreeMap::new();
    for step in report
        .steps
        .iter()
        .filter(|step| merged_with_failed_deploy(step))
    {
        let Some((slug, id)) = failed_run_url(step).and_then(|url| parse_run(&url)) else {
            continue;
        };
        let path = format!("/repos/{slug}/actions/runs/{id}/jobs?per_page=100&filter=latest");
        if let Ok(value) = client.rest(&path)
            && let Some(mut text) = describe_jobs(&value)
        {
            let known = [step.deploy.as_deref(), Some(step.detail.as_str())]
                .into_iter()
                .flatten()
                .any(|text| text.to_lowercase().contains(ACTIVE_DEPLOYMENT));
            if !known
                && let Some(job) = failed_job_id(&value)
                && let Some(message) = active_deployment_annotation(client, &slug, job)
            {
                text.push_str(&format!("; {message}"));
            }
            details.insert(step.target.clone(), text);
        }
    }
    details
}

fn failed_job_id(value: &Value) -> Option<u64> {
    value
        .get("jobs")?
        .as_array()?
        .iter()
        .find(|job| {
            matches!(
                job.get("conclusion").and_then(Value::as_str),
                Some("failure" | "timed_out" | "cancelled")
            )
        })?
        .get("id")?
        .as_u64()
}

fn active_deployment_annotation(client: &mut Client, slug: &str, job: u64) -> Option<String> {
    let value = client
        .rest(&format!(
            "/repos/{slug}/check-runs/{job}/annotations?per_page=50"
        ))
        .ok()?;
    value.as_array()?.iter().find_map(|annotation| {
        ["message", "title"]
            .into_iter()
            .filter_map(|field| annotation.get(field).and_then(Value::as_str))
            .find(|text| text.to_lowercase().contains(ACTIVE_DEPLOYMENT))
            .map(|text| truncate(&text.replace(['\n', '\r'], " "), 160))
    })
}
