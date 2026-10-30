use serde_json::{Value, json};

use super::check::{Check, Status};
use crate::style;

pub const SCHEMA: u64 = 1;

fn tag(status: Status) -> String {
    match status {
        Status::Ok => style::green(" ok "),
        Status::Warn => style::yellow("warn"),
        Status::Fail => style::red("fail"),
    }
}

fn fix_lines(fix: &str) -> String {
    fix.lines()
        .map(|line| style::dim(&format!("       {line}")))
        .collect::<Vec<_>>()
        .join("\n")
}

fn line(check: &Check) -> String {
    let head = format!(
        "[{}] {} {}",
        tag(check.status),
        style::bold(&check.id),
        check.message
    );
    match (&check.fix, check.status) {
        (Some(fix), Status::Warn | Status::Fail) => format!("{head}\n{}\n", fix_lines(fix)),
        _ => format!("{head}\n"),
    }
}

pub fn text(checks: &[Check]) -> String {
    let mut out: String = checks.iter().map(line).collect();
    out.push_str(&summary(checks));
    out
}

fn summary(checks: &[Check]) -> String {
    let count = |status| checks.iter().filter(|c| c.status == status).count();
    format!(
        "{} ok, {} warn, {} fail\n",
        count(Status::Ok),
        count(Status::Warn),
        count(Status::Fail)
    )
}

pub fn json(checks: &[Check]) -> String {
    let value: Value = json!({ "schema": SCHEMA, "checks": checks });
    serde_json::to_string_pretty(&value).expect("checks always serialize")
}

pub fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|c| c.status == Status::Fail))
}
