use std::collections::{BTreeMap, BTreeSet};

use clap::ValueEnum;

use crate::bundle::Bundle;
use crate::plan::Plan;
use crate::planner::{Item, Rule};

pub const PAUSE_LINE: &str = "# pause: confirm DDL/infra before continuing";
pub const EXIT_QUESTIONS: i32 = 3;
const REPO_LEVEL: &str = "(repo)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Emit {
    Queue,
}

fn wait_lines(plan: &Plan, item: &Item) -> Vec<String> {
    let mut waits: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for dep in item.after.iter().filter(|d| d.rule == Rule::Package) {
        let Some(package) = dep.package.as_deref() else {
            continue;
        };
        if plan.items.iter().any(|i| i.id == dep.id) {
            waits.entry(package).or_default().insert(dep.id.as_str());
        }
    }
    waits
        .into_iter()
        .map(|(package, publishers)| {
            let from: Vec<&str> = publishers.into_iter().collect();
            format!(
                "# wait: package {package} from {} must be published before {}",
                from.join(" "),
                item.id
            )
        })
        .collect()
}

fn item_lines(plan: &Plan, item: &Item) -> Vec<String> {
    let mut lines = wait_lines(plan, item);
    lines.push(format!("merge:{}", item.id));
    lines
}

fn order_key(item: &Item) -> (usize, usize, u64, &str, &str) {
    (
        item.stage,
        item.layer_rank.unwrap_or(usize::MAX),
        item.number,
        &item.org,
        &item.repo,
    )
}

fn flattened(plan: &Plan) -> Vec<&Item> {
    let mut rest: Vec<&Item> = plan.items.iter().filter(|i| !i.manual_pause).collect();
    rest.sort_by_key(|i| order_key(i));
    rest
}

fn gated(plan: &Plan) -> Vec<&Item> {
    let mut gate: Vec<&Item> = plan
        .gate
        .iter()
        .filter_map(|id| plan.items.iter().find(|i| i.id == *id))
        .collect();
    gate.sort_by_key(|i| order_key(i));
    gate
}

fn bundle_line(bundle: &Bundle) -> String {
    let ids: Vec<&str> = bundle
        .existing_train_pr
        .iter()
        .chain(bundle.retarget.iter())
        .map(String::as_str)
        .collect();
    format!(
        "# bundle {}: {} PRs into {} could be 1 deploy (saves {}): {}",
        bundle.repo,
        bundle.deploys_now,
        bundle.base,
        bundle.saved,
        ids.join(" ")
    )
}

pub fn queue_lines(plan: &Plan) -> Vec<String> {
    let mut lines: Vec<String> = gated(plan)
        .into_iter()
        .flat_map(|i| item_lines(plan, i))
        .collect();
    let rest = flattened(plan);
    if !lines.is_empty() && !rest.is_empty() {
        lines.push(PAUSE_LINE.to_string());
    }
    lines.extend(rest.into_iter().flat_map(|i| item_lines(plan, i)));
    if lines.is_empty() {
        return lines;
    }
    let mut out: Vec<String> = plan.bundles.iter().map(bundle_line).collect();
    out.extend(lines);
    out
}

pub fn question_lines(plan: &Plan) -> Vec<String> {
    plan.questions
        .iter()
        .map(|q| format!("? {}: {}", q.pr.as_deref().unwrap_or(REPO_LEVEL), q.message))
        .collect()
}

pub fn exit_code(plan: &Plan) -> i32 {
    if plan.questions.is_empty() {
        0
    } else {
        EXIT_QUESTIONS
    }
}
