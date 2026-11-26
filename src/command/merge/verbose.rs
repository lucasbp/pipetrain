use super::{KEPT, TRAIN_NOTICE, admin, compact, is_kept};
use crate::command::apply::tint;
use crate::merge::{Kind, Report, Status, StepReport, category};
use crate::style::{bold, dim, green, red, yellow};

pub(super) fn step_text(step: &StepReport, color: bool) -> String {
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
            let settling = step
                .settle_seconds
                .map(|seconds| format!("; settling {seconds}s"))
                .unwrap_or_default();
            format!("{what}{suffix}{deploy}{prediction}{settling}")
        }
        Status::Done => format!("{} {what}{suffix}{deploy}", tint(color, green, "done:")),
        Status::Skipped if step.kind == Kind::Bump && step.blocked_by.is_none() => {
            format!("{} {what}{suffix}", tint(color, yellow, "waiting:"))
        }
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

pub(super) fn usage_line(usage: &crate::github::Usage) -> String {
    if usage.spent() == 0 && usage.cache_hits == 0 && usage.not_modified == 0 {
        return String::new();
    }
    format!(
        "API used: core {}, graphql {}, search {}; {} from cache, {} not modified\n",
        usage.core, usage.graphql, usage.search, usage.cache_hits, usage.not_modified
    )
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
        for notice in &report.notices {
            out.push_str(&format!("{}\n", tint(color, yellow, notice)));
        }
        let estimate = &report.api_estimate;
        out.push_str(&format!("estimated API requests: {} (discovery {}, checks and merges {}, deploy polls {}); budget {}\n", estimate.total, estimate.discovery, estimate.checks, estimate.deploy_polls, estimate.budget));
        if let Some(warning) = estimate.warning() {
            out.push_str(&format!("{warning}\n"));
        }
        for pr in &report.preflight {
            for line in pr.lines() {
                out.push_str(&format!("{line}\n"));
            }
        }
    }
    if report.bump_enabled && include_steps {
        out.push_str("--bump enabled: with --apply this run may create isolated bump branches and draft PRs targeting consumer heads or a shared base, then block those consumers for CI and review. Dry runs perform no writes.\n");
        for target in &report.bump_candidates {
            out.push_str(&format!("potential dependency bump: {target} (only after a new publication is verified; no version chosen yet)\n"));
        }
        out.push('\n');
    }
    out.push_str(&usage_line(&report.api_usage));
    if report.steps.is_empty() {
        out.push_str(&format!("{}\n", tint(color, dim, "nothing to merge")));
    }
    if include_steps {
        if report.requested && !report.applied {
            out.push_str(&admin::notice(&report.steps));
        }
        let overridden: Vec<_> = report
            .steps
            .iter()
            .filter(|step| step.kind == Kind::Merge && step.admin_override.is_some())
            .collect();
        if !report.requested && !overridden.is_empty() {
            out.push_str(&format!(
                "--admin: {} PR(s) would be merged past a missing review:\n",
                overridden.len()
            ));
            for step in overridden {
                out.push_str(&format!("  {}\n", step.target));
            }
        }
        for step in &report.steps {
            out.push_str(&format!("{}\n", step_text(step, color)));
            if let Some(admin) = &step.admin_override {
                out.push_str(&format!(
                    "  admin override: merges past {} as {}\n",
                    admin.reason, admin.requested_by
                ));
            }
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
            let label = skipped.state.as_str();
            let label = match skipped.state {
                category::Category::Waiting => tint(color, yellow, label),
                category::Category::Blocked => tint(color, red, label),
            };
            out.push_str(&format!(
                "  {} ({label}): {}\n",
                skipped.id,
                skipped.reasons.join(", ")
            ));
        }
        out.push_str(&format!("{}\n", compact::footer_counts(report, color)));
    }
    for question in &report.questions {
        out.push_str(&format!("{question}\n"));
    }
    if let Some(reason) = &report.stopped {
        out.push_str(&format!("\n{}\n", tint(color, yellow, reason)));
    }
    if report.applied {
        for step in &report.steps {
            if step.merge_sha.is_some()
                && let Some(admin) = &step.admin_override
                && admin.at.is_some()
            {
                out.push_str(&format!(
                    "merged by admin override: {} ({})\n",
                    step.target, admin.reason
                ));
            }
        }
        for step in report.steps.iter().filter(|step| is_kept(step)) {
            out.push_str(&format!("{KEPT}: {}\n", step.target));
        }
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
