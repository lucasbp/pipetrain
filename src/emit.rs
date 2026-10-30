use clap::ValueEnum;

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

fn flattened(plan: &Plan) -> Vec<&Item> {
    let mut rest: Vec<&Item> = plan.items.iter().filter(|i| !i.manual_pause).collect();
    rest.sort_by_key(|i| i.stage);
    rest
}

pub fn queue_lines(plan: &Plan) -> Vec<String> {
    let mut lines: Vec<String> = plan.gate.iter().map(|id| item_line(id)).collect();
    let rest = flattened(plan);
    if !lines.is_empty() && !rest.is_empty() {
        lines.push(PAUSE_LINE.to_string());
    }
    lines.extend(rest.into_iter().map(|i| item_line(&i.id)));
    lines
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
