use clap::ValueEnum;

use crate::bundle::Bundle;
use crate::plan::Plan;
use crate::planner::Item;

pub const PAUSE_LINE: &str = "# pause: confirm DDL/infra before continuing";
pub const EXIT_QUESTIONS: i32 = 3;
const REPO_LEVEL: &str = "(repo)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Emit {
    Queue,
}

fn item_line(id: &str) -> String {
    format!("merge:{id}")
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

fn gate_ids(plan: &Plan) -> Vec<&str> {
    let mut gate: Vec<&Item> = plan
        .gate
        .iter()
        .filter_map(|id| plan.items.iter().find(|i| i.id == *id))
        .collect();
    gate.sort_by_key(|i| order_key(i));
    gate.into_iter().map(|i| i.id.as_str()).collect()
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
    let mut lines: Vec<String> = gate_ids(plan).into_iter().map(item_line).collect();
    let rest = flattened(plan);
    if !lines.is_empty() && !rest.is_empty() {
        lines.push(PAUSE_LINE.to_string());
    }
    lines.extend(rest.into_iter().map(|i| item_line(&i.id)));
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
