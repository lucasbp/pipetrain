use std::collections::{BTreeMap, BTreeSet};

use super::failure::DeployFailures;
use crate::config::GateKind;
use crate::merge::{Kind, StepReport};
use crate::plan::Plan;

pub const BATCH: &str = "Gates to confirm before any merge";
pub const ITEM: &str = "Gate ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Confirmed,
    Refused,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Answers {
    covered: BTreeMap<String, Vec<String>>,
    refused: BTreeSet<String>,
    flag: bool,
}

pub(super) struct Listed {
    pub id: String,
    pub kind: &'static str,
    pub reason: String,
}

fn kind_label(kind: GateKind) -> &'static str {
    match kind {
        GateKind::Ddl => "DDL",
        GateKind::Infra => "infra",
    }
}

pub(super) fn listed(plan: &Plan, steps: &[StepReport]) -> Vec<Listed> {
    steps
        .iter()
        .filter(|step| step.kind == Kind::Merge)
        .filter_map(|step| plan.items.iter().find(|item| item.id == step.target))
        .filter(|item| !item.hidden)
        .filter_map(|item| {
            item.gate.map(|kind| Listed {
                id: item.id.clone(),
                kind: kind_label(kind),
                reason: item.gate_reason.clone().unwrap_or_default(),
            })
        })
        .collect()
}

fn line(gate: &Listed) -> String {
    format!("{} ({}): {}", gate.id, gate.kind, gate.reason)
}

fn batch_text(gates: &[Listed]) -> String {
    let lines: Vec<String> = gates
        .iter()
        .map(|gate| format!("  {}", line(gate)))
        .collect();
    format!("{BATCH} ({}):\n{}", gates.len(), lines.join("\n"))
}

pub(super) fn preview(
    plan: &Plan,
    steps: &[StepReport],
    confirmed_by_flag: bool,
) -> Option<String> {
    let gates = listed(plan, steps);
    if gates.is_empty() {
        return None;
    }
    let names: Vec<String> = gates.iter().map(line).collect();
    let how = if confirmed_by_flag {
        "confirmed by --gates-confirmed, nothing is asked"
    } else {
        "--apply asks about each before any merge; one that is not confirmed holds only its own branch"
    };
    Some(format!("gates: {}; {how}", names.join("; ")))
}

fn covered(plan: &Plan, steps: &[StepReport], index: usize) -> Vec<String> {
    let late = steps[index + 1..]
        .iter()
        .find(|step| step.kind == Kind::Merge)
        .and_then(|next| plan.items.iter().find(|item| item.id == next.target))
        .filter(|item| {
            item.gate_late
                && steps[index]
                    .target
                    .starts_with(&format!("{} waits for ", item.id))
        });
    if let Some(item) = late {
        return vec![item.id.clone()];
    }
    steps[..index]
        .iter()
        .filter(|step| step.kind == Kind::Merge)
        .filter_map(|step| plan.items.iter().find(|item| item.id == step.target))
        .filter(|item| item.gate.is_some() && !item.gate_late && !item.hidden)
        .map(|item| item.id.clone())
        .collect()
}

pub(super) fn ask(
    plan: &Plan,
    steps: &[StepReport],
    confirmed_by_flag: bool,
    hook: &mut dyn FnMut(&str) -> bool,
) -> Answers {
    let gates = listed(plan, steps);
    let mut answers = Answers {
        flag: confirmed_by_flag,
        ..Answers::default()
    };
    if gates.is_empty() {
        return answers;
    }
    for (index, step) in steps.iter().enumerate() {
        if step.kind != Kind::Gate {
            continue;
        }
        let ids = covered(plan, steps, index);
        if !ids.is_empty() {
            answers.covered.insert(step.target.clone(), ids);
        }
    }
    if confirmed_by_flag {
        return answers;
    }
    let all = gates.len() > 1 && hook(&batch_text(&gates));
    for gate in &gates {
        if !all && !hook(&format!("{ITEM}{}", line(gate))) {
            answers.refused.insert(gate.id.clone());
        }
    }
    answers
}

impl Answers {
    pub(super) fn verdict(&self, target: &str) -> Option<Verdict> {
        let ids = self.covered.get(target)?;
        if ids.iter().any(|id| self.refused.contains(id)) {
            Some(Verdict::Refused)
        } else {
            Some(Verdict::Confirmed)
        }
    }

    pub(super) fn covers(&self, target: &str) -> Option<&[String]> {
        self.covered.get(target).map(Vec::as_slice)
    }

    pub(super) fn by_flag(&self) -> bool {
        self.flag
    }

    pub(super) fn hold(&self, plan: &Plan, failures: &mut DeployFailures) {
        for item in plan
            .items
            .iter()
            .filter(|item| self.refused.contains(&item.id))
        {
            failures.record_gate(item, plan);
        }
    }
}
