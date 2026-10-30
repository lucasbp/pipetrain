use std::collections::HashMap;
use std::fmt::Write;

use crate::plan::Plan;
use crate::planner::{Item, Rule};

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn item_line(out: &mut String, item: &Item, marked: bool, width: usize, with_stage: bool) {
    let mark = if marked { "?" } else { " " };
    let stage = if with_stage {
        format!("s{}  ", item.stage)
    } else {
        String::new()
    };
    let layer = item
        .layer
        .as_deref()
        .map(|l| format!("[{l}] "))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "  {stage}{mark} {:<width$}  {layer}{}",
        item.id,
        clip(&item.title, 56)
    );
}

fn after_lines(out: &mut String, item: &Item, indent: &str) {
    for dep in item
        .after
        .iter()
        .filter(|d| !d.implied && d.rule != Rule::Gate)
    {
        let _ = writeln!(
            out,
            "{indent}after {}  ({}: {})",
            dep.id,
            dep.rule.as_str(),
            dep.reason
        );
    }
}

pub fn text(plan: &Plan) -> String {
    let mut out = String::new();
    let by_id: HashMap<&str, &Item> = plan.items.iter().map(|i| (i.id.as_str(), i)).collect();
    let marked: HashMap<&str, usize> = plan.questions.iter().filter_map(|q| q.pr.as_deref()).fold(
        HashMap::new(),
        |mut acc, id| {
            *acc.entry(id).or_default() += 1;
            acc
        },
    );
    let width = plan.items.iter().map(|i| i.id.len()).max().unwrap_or(0);

    let _ = writeln!(out, "pipetrain plan  (read-only, nothing was merged)");
    let _ = writeln!(out, "  train     {}", plan.train);
    let _ = writeln!(out, "  orgs      {}", plan.orgs.join(", "));
    let _ = writeln!(
        out,
        "  found     {} ready, {} skipped, {} questions",
        plan.items.len(),
        plan.skipped.len(),
        plan.questions.len()
    );
    let _ = writeln!(
        out,
        "  parallel  peak {} at once (cap {})",
        plan.stats.peak_parallel, plan.stats.max_parallel
    );
    let _ = writeln!(
        out,
        "  deploys   {} now, {} after bundling",
        plan.stats.deploys_now, plan.stats.deploys_after
    );
    let _ = writeln!(
        out,
        "  api       {} requests, {} left",
        plan.stats.requests,
        plan.stats
            .rate_remaining
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string())
    );

    if plan.items.is_empty() {
        let _ = writeln!(out, "\nNothing ready to plan.");
        if plan.train != "explicit PRs" && plan.train != "all released PRs" {
            let _ = writeln!(
                out,
                "Tip: nothing is on {} today. Run with --all to plan every approved PR across the orgs.",
                plan.train
            );
        }
    }

    if !plan.gate.is_empty() {
        let _ = writeln!(out, "\nGATE  runs first, pauses for your confirmation");
        for id in &plan.gate {
            if let Some(item) = by_id.get(id.as_str()) {
                item_line(
                    &mut out,
                    item,
                    marked.contains_key(id.as_str()),
                    width,
                    false,
                );
                let kind = match item.gate {
                    Some(crate::config::GateKind::Ddl) => "DDL",
                    Some(crate::config::GateKind::Infra) => "infra",
                    None => "",
                };
                let reason = item.gate_reason.as_deref().unwrap_or("");
                let _ = writeln!(
                    out,
                    "        {kind} ({reason}): every lane below waits for this"
                );
                after_lines(&mut out, item, "        ");
            }
        }
    }

    for (n, lane) in plan.lanes.iter().enumerate() {
        let _ = writeln!(out, "\nLANE {}", n + 1);
        for id in lane {
            if let Some(item) = by_id.get(id.as_str()) {
                item_line(
                    &mut out,
                    item,
                    marked.contains_key(id.as_str()),
                    width,
                    true,
                );
                after_lines(&mut out, item, "          ");
            }
        }
    }

    if !plan.bundles.is_empty() {
        let _ = writeln!(
            out,
            "\nBUNDLES  one deploy branch instead of N deploys (suggestion, nothing was changed)"
        );
        for b in &plan.bundles {
            let ids = b.retarget.join(", ");
            let _ = writeln!(
                out,
                "  {}  {} PRs into {} -> 1 deploy, saves {}",
                b.repo, b.deploys_now, b.base, b.saved
            );
            match &b.existing_train_pr {
                None => {
                    let _ = writeln!(
                        out,
                        "        create {} from {}, retarget {ids}, merge them there, then open one PR {} -> {}",
                        b.branch, b.base, b.branch, b.base
                    );
                }
                Some(existing) => {
                    let _ = writeln!(
                        out,
                        "        retarget {ids} to {} and merge them there; {existing} then carries all of them",
                        b.branch
                    );
                }
            }
        }
    }

    if !plan.skipped.is_empty() {
        let _ = writeln!(out, "\nSKIPPED  not ready, left out of the plan");
        for s in &plan.skipped {
            let _ = writeln!(
                out,
                "  {}  {}  [{}: {}]  ({})",
                s.id,
                clip(&s.title, 40),
                s.scope,
                clip(&s.head, 32),
                s.reasons.join(", ")
            );
        }
    }

    if !plan.questions.is_empty() {
        let _ = writeln!(out, "\n?  QUESTIONS  the planner could not decide these");
        for q in &plan.questions {
            let who = q.pr.as_deref().unwrap_or("-");
            let _ = writeln!(out, "  {who}  [{}]  {}", q.code, q.message);
            let _ = writeln!(out, "        fix: {}", q.hint);
        }
    }

    if !plan.notes.is_empty() {
        let _ = writeln!(out, "\nNOTES");
        for note in &plan.notes {
            let _ = writeln!(out, "  {note}");
        }
    }
    out
}
