use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::record::Record;

pub const SCHEMA: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStats {
    pub command: String,
    pub runs: usize,
    pub failed: usize,
    pub median_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorStats {
    pub error: String,
    pub count: usize,
    pub per_thousand: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub runs: usize,
    pub median_ms: u64,
    pub commands: Vec<CommandStats>,
    pub errors: Vec<ErrorStats>,
}

pub fn median(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2
    }
}

fn label(record: &Record) -> Option<String> {
    let kind = record.error.as_deref()?;
    Some(match record.variant.as_deref() {
        Some(variant) => format!("{kind}::{variant}"),
        None => kind.to_string(),
    })
}

pub fn build(records: &[Record]) -> Summary {
    let mut by_command: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    for record in records {
        by_command.entry(&record.command).or_default().push(record);
        if let Some(found) = label(record) {
            *errors.entry(found).or_default() += 1;
        }
    }
    let commands = by_command
        .into_iter()
        .map(|(command, rows)| {
            let mut durations: Vec<u64> = rows.iter().map(|r| r.duration_ms).collect();
            CommandStats {
                command: command.to_string(),
                runs: rows.len(),
                failed: rows.iter().filter(|r| r.exit != 0).count(),
                median_ms: median(&mut durations),
            }
        })
        .collect();
    let total = records.len();
    let mut errors: Vec<ErrorStats> = errors
        .into_iter()
        .map(|(error, count)| ErrorStats {
            error,
            count,
            per_thousand: count * 1000 / total.max(1),
        })
        .collect();
    errors.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.error.cmp(&b.error)));
    let mut all: Vec<u64> = records.iter().map(|r| r.duration_ms).collect();
    Summary {
        runs: total,
        median_ms: median(&mut all),
        commands,
        errors,
    }
}

pub fn text(summary: &Summary) -> String {
    let mut out = format!("{} runs, median {} ms\n", summary.runs, summary.median_ms);
    for item in &summary.commands {
        out.push_str(&format!(
            "  {:<10} {:>5} runs  {:>4} failed  median {} ms\n",
            item.command, item.runs, item.failed, item.median_ms
        ));
    }
    if summary.errors.is_empty() {
        out.push_str("no typed errors\n");
    } else {
        out.push_str("errors by type:\n");
        for item in &summary.errors {
            out.push_str(&format!(
                "  {:<28} {:>5}  ({}.{}% of runs)\n",
                item.error,
                item.count,
                item.per_thousand / 10,
                item.per_thousand % 10
            ));
        }
    }
    out
}

pub fn json(summary: &Summary) -> Value {
    json!({
        "schema": SCHEMA,
        "runs": summary.runs,
        "median_ms": summary.median_ms,
        "commands": summary.commands.iter().map(|c| json!({
            "command": c.command,
            "runs": c.runs,
            "failed": c.failed,
            "median_ms": c.median_ms,
        })).collect::<Vec<_>>(),
        "errors": summary.errors.iter().map(|e| json!({
            "error": e.error,
            "count": e.count,
            "rate": e.count as f64 / summary.runs.max(1) as f64,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::stats::Counts;

    fn row(command: &str, exit: i32, variant: Option<&str>, ms: u64) -> Record {
        let mut record = crate::log::build(command, exit, None, ms, Counts::default());
        if let Some(variant) = variant {
            record.error = Some("ApiError".to_string());
            record.variant = Some(variant.to_string());
        }
        record
    }

    fn sample() -> Vec<Record> {
        vec![
            row("plan", 0, None, 100),
            row("plan", 0, None, 300),
            row("plan", 1, Some("RateLimited"), 200),
            row("doctor", 1, None, 50),
        ]
    }

    #[test]
    fn the_median_of_an_even_count_is_the_mean_of_the_middle_pair() {
        assert_eq!(median(&mut [4, 1, 3, 2]), 2);
        assert_eq!(median(&mut [5, 1, 3]), 3);
        assert_eq!(median(&mut []), 0);
    }

    #[test]
    fn it_counts_per_command_with_failures_and_median() {
        let summary = build(&sample());
        assert_eq!(summary.runs, 4);
        let plan = &summary.commands[1];
        assert_eq!(plan.command, "plan");
        assert_eq!((plan.runs, plan.failed, plan.median_ms), (3, 1, 200));
        assert_eq!(summary.commands[0].command, "doctor");
        assert_eq!(summary.median_ms, 150);
    }

    #[test]
    fn it_rates_errors_by_type_over_all_runs() {
        let summary = build(&sample());
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.errors[0].error, "ApiError::RateLimited");
        assert_eq!(summary.errors[0].per_thousand, 250);
        assert!(text(&summary).contains("25.0% of runs"));
    }

    #[test]
    fn the_json_has_schema_one() {
        let value = json(&build(&sample()));
        assert_eq!(value["schema"], 1);
        assert_eq!(value["runs"], 4);
        assert_eq!(value["errors"][0]["count"], 1);
        assert_eq!(value["errors"][0]["rate"], 0.25);
        assert_eq!(value["commands"][1]["command"], "plan");
    }

    #[test]
    fn an_empty_log_summarizes_to_zero() {
        let summary = build(&[]);
        assert_eq!(summary.runs, 0);
        assert!(text(&summary).starts_with("0 runs"));
    }
}
