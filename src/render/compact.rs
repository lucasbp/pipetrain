use std::collections::HashMap;
use std::fmt::Write;

use super::{Names, clip, feed_checks, handle};
use crate::config::GateKind;
use crate::merge::category::{self, Category};
use crate::plan::Plan;
use crate::planner::Item;
use crate::style::{bold, dim, red, yellow};

fn primary_cause(reasons: &[String], state: Category) -> String {
    let first = reasons
        .iter()
        .find(|reason| category::classify(std::slice::from_ref(reason)) == state)
        .or_else(|| reasons.first())
        .map_or("not ready", String::as_str);
    if first.starts_with("checks failing: ") {
        "checks failing".to_string()
    } else {
        clip(first, 90)
    }
}

fn step_note(item: &Item, names: &Names, held: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(kind) = item.gate {
        parts.push(yellow(match kind {
            GateKind::Ddl => "confirm DDL",
            GateKind::Infra => "confirm infra",
        }));
    }
    if let Some(resource) = held {
        parts.push(format!("wait for shared deploy resource {resource}"));
    }
    let mut direct = item.after.iter().filter(|dep| !dep.implied);
    if let Some(dep) = direct.next() {
        let reason = dep.package.as_deref().unwrap_or(dep.rule.as_str());
        let mut wait = format!("after {} ({reason})", names.short(&dep.id));
        let more = direct.count();
        if more > 0 {
            let _ = write!(wait, " +{more}");
        }
        parts.push(wait);
    }
    parts.join(", ")
}

pub fn compact(plan: &Plan) -> String {
    let mut out = String::new();
    let names = Names::new(plan);
    let mut items: Vec<_> = plan.items.iter().filter(|item| !item.hidden).collect();
    items.sort_by_key(|item| (!item.manual_pause || item.gate_late, item.stage));
    let skipped: Vec<_> = plan.skipped.iter().filter(|item| !item.hidden).collect();
    let questions: Vec<_> = plan.questions.iter().filter(|q| !q.hidden).collect();
    let profile = plan.profile.as_ref().map_or("", |info| info.name.as_str());
    let scope = [profile, &plan.train, &plan.orgs.join(", ")]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let _ = writeln!(out, "{}  {}", bold("pipetrain plan"), dim(&scope));
    let _ = writeln!(
        out,
        "{} in plan · {} skipped · {} questions",
        items.len(),
        skipped.len(),
        questions.len()
    );
    if plan.stats.hidden_by_author > 0 {
        let _ = writeln!(out, "{} hidden by author", plan.stats.hidden_by_author);
    }
    if items.is_empty() {
        let _ = writeln!(out, "Nothing ready to plan.");
    }
    let held: HashMap<_, _> = plan
        .stages
        .iter()
        .flat_map(|stage| &stage.held)
        .map(|wait| (wait.id.as_str(), wait.detail.as_str()))
        .collect();
    for item in items {
        let _ = write!(
            out,
            "  step {}  {}{}  {}",
            item.stage,
            bold(&names.short(&item.id)),
            handle(&item.author),
            clip(&item.title, 52)
        );
        let note = step_note(item, &names, held.get(item.id.as_str()).copied());
        if !note.is_empty() {
            let _ = write!(out, "  {note}");
        }
        out.push('\n');
    }
    feed_checks(&mut out, plan);
    for item in skipped {
        let category = category::classify(&item.reasons);
        let state = match category {
            Category::Waiting => yellow("waiting"),
            Category::Blocked => red("blocked"),
        };
        let _ = writeln!(
            out,
            "  {state}  {}{}  {}",
            bold(&names.short(&item.id)),
            handle(&item.author),
            primary_cause(&item.reasons, category)
        );
    }
    for question in questions {
        let who = question
            .pr
            .as_deref()
            .map(|id| format!("{}  ", names.short(id)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {}  {who}{}  {}",
            red("?"),
            question.message,
            question.hint
        );
    }
    for pr in plan
        .preflight
        .iter()
        .filter(|pr| !pr.warnings.is_empty() || !pr.exceptions.is_empty())
    {
        if plan.is_hidden(&pr.id) {
            continue;
        }
        for warning in &pr.warnings {
            let _ = writeln!(
                out,
                "  {}  {}  {warning}",
                yellow("warning"),
                names.short(&pr.id)
            );
        }
        for exception in &pr.exceptions {
            let _ = writeln!(
                out,
                "  {}  {}  {exception}",
                yellow("exception"),
                names.short(&pr.id)
            );
        }
    }
    let conflicts = plan
        .conflicts
        .iter()
        .filter(|c| !plan.is_hidden(&c.a) || !plan.is_hidden(&c.b))
        .count();
    if conflicts > 0 {
        let _ = writeln!(out, "PR pairs with overlapping files: {conflicts}");
    }
    if plan.stats.deploys_saved > 0 {
        let _ = writeln!(
            out,
            "Deploys saved by suggested bundles: {}",
            plan.stats.deploys_saved
        );
    }
    let _ = writeln!(
        out,
        "{}",
        dim("Use --verbose for the full plan, causes and checks.")
    );
    out
}
