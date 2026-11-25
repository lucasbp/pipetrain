use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use serde::Serialize;

use crate::deploy_resources;
use crate::planner::{Item, Rule};

pub const RESOURCE: &str = "deploy-resource";
pub const GATE: &str = "gate";
pub const PACKAGE: &str = "package";
pub const SAME_REPO: &str = "same-repo";
pub const DEPENDENCY: &str = "dependency";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wait {
    pub id: String,
    pub reason: String,
    pub detail: String,
    pub behind: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateNote {
    pub id: String,
    pub kind: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageView {
    pub stage: usize,
    pub together: Vec<String>,
    pub waits: Vec<Wait>,
    pub held: Vec<Wait>,
    pub gates: Vec<GateNote>,
}

fn reason_of(rule: Rule) -> &'static str {
    match rule {
        Rule::Gate => GATE,
        Rule::Package => PACKAGE,
        Rule::Target | Rule::Stacked => SAME_REPO,
        _ => DEPENDENCY,
    }
}

fn detail_of(item: &Item, dep: &crate::planner::Dependency) -> String {
    match dep.rule {
        Rule::Package => dep.package.clone().unwrap_or_default(),
        Rule::Target | Rule::Stacked => format!("{}/{}", item.org, item.repo),
        _ => dep.rule.as_str().to_string(),
    }
}

pub const GATE_NOTE: &str = "runs with this stage once confirmed at --apply start";

fn gate_kind(item: &Item) -> &'static str {
    match item.gate {
        Some(crate::config::GateKind::Ddl) => "DDL",
        _ => "infra",
    }
}

pub fn compute(
    items: &[Item],
    detected: &BTreeMap<String, Vec<String>>,
    configured: &BTreeMap<String, usize>,
) -> Vec<StageView> {
    let mut views = place(items, detected, configured, false);
    for item in items
        .iter()
        .filter(|item| !item.hidden && item.gate.is_some())
    {
        let note = GateNote {
            id: item.id.clone(),
            kind: gate_kind(item).to_string(),
            note: GATE_NOTE.to_string(),
        };
        match views.iter_mut().find(|view| view.stage == item.stage) {
            Some(view) => view.gates.push(note),
            None => views.push(StageView {
                stage: item.stage,
                together: Vec::new(),
                waits: Vec::new(),
                held: Vec::new(),
                gates: vec![note],
            }),
        }
    }
    views.sort_by_key(|view| view.stage);
    views
}

pub fn effective_peak_if_gates_confirmed(
    items: &[Item],
    detected: &BTreeMap<String, Vec<String>>,
    configured: &BTreeMap<String, usize>,
    max_parallel: usize,
) -> usize {
    effective_peak(&place(items, detected, configured, true), max_parallel)
}

fn place(
    items: &[Item],
    detected: &BTreeMap<String, Vec<String>>,
    configured: &BTreeMap<String, usize>,
    include_gates: bool,
) -> Vec<StageView> {
    let running: Vec<&Item> = items
        .iter()
        .filter(|item| !item.hidden && (include_gates || item.gate.is_none()))
        .collect();
    let jobs: Vec<(String, String)> = running
        .iter()
        .map(|item| (item.id.clone(), format!("{}/{}", item.org, item.repo)))
        .collect();
    let capacity = deploy_resources::capacity(&jobs, detected, configured);
    let mut by_stage: BTreeMap<usize, Vec<&Item>> = BTreeMap::new();
    for item in &running {
        by_stage.entry(item.stage).or_default().push(item);
    }
    let mut views = Vec::new();
    for (stage, mut members) in by_stage {
        members.sort_by(|a, b| a.id.cmp(&b.id));
        let mut taken: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut together = Vec::new();
        let mut held = Vec::new();
        for item in &members {
            let needs = capacity
                .resources
                .get(&item.id)
                .map_or(&[][..], Vec::as_slice);
            let blocked = needs.iter().find(|resource| {
                let limit = capacity
                    .slots
                    .get(resource.as_str())
                    .copied()
                    .unwrap_or(1)
                    .max(1);
                let users = taken.get(resource.as_str()).map_or(0, Vec::len);
                users >= limit
            });
            match blocked {
                Some(resource) => held.push(Wait {
                    id: item.id.clone(),
                    reason: RESOURCE.to_string(),
                    detail: resource.clone(),
                    behind: taken
                        .get(resource.as_str())
                        .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                        .unwrap_or_default(),
                }),
                None => {
                    for resource in needs {
                        taken.entry(resource).or_default().push(&item.id);
                    }
                    together.push(item.id.clone());
                }
            }
        }
        let mut waits = Vec::new();
        for item in &members {
            let mut grouped: BTreeMap<(&'static str, String), BTreeSet<String>> = BTreeMap::new();
            for dep in item.after.iter().filter(|dep| !dep.implied) {
                grouped
                    .entry((reason_of(dep.rule), detail_of(item, dep)))
                    .or_default()
                    .insert(dep.id.clone());
            }
            for ((reason, detail), behind) in grouped {
                waits.push(Wait {
                    id: item.id.clone(),
                    reason: reason.to_string(),
                    detail,
                    behind: behind.into_iter().collect(),
                });
            }
        }
        views.push(StageView {
            stage,
            together,
            waits,
            held,
            gates: Vec::new(),
        });
    }
    views
}

pub fn effective_peak(views: &[StageView], max_parallel: usize) -> usize {
    let widest = views
        .iter()
        .map(|view| view.together.len())
        .max()
        .unwrap_or(0);
    if max_parallel == 0 {
        widest
    } else {
        widest.min(max_parallel)
    }
}

fn names(ids: &[String], short: &dyn Fn(&str) -> String) -> String {
    ids.iter()
        .map(|id| short(id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn explain(wait: &Wait, short: &dyn Fn(&str) -> String) -> String {
    let behind = names(&wait.behind, short);
    match wait.reason.as_str() {
        RESOURCE => format!(
            "{} waits for the shared deploy resource {} (in use by {behind})",
            short(&wait.id),
            wait.detail
        ),
        GATE => format!("{} waits for the gate of {behind}", short(&wait.id)),
        PACKAGE => format!(
            "{} waits for package {} published by {behind}",
            short(&wait.id),
            wait.detail
        ),
        SAME_REPO => format!(
            "{} waits for {behind}, same repo {}",
            short(&wait.id),
            wait.detail
        ),
        _ => format!("{} waits for {behind} ({})", short(&wait.id), wait.detail),
    }
}

pub fn lines(views: &[StageView], short: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    for view in views {
        let mut line = String::new();
        let _ = write!(
            line,
            "stage {}: {}",
            view.stage,
            if view.together.is_empty() {
                "nothing runs".to_string()
            } else {
                format!("together {}", names(&view.together, short))
            }
        );
        out.push(line);
        for wait in view.held.iter().chain(view.waits.iter()) {
            out.push(format!("  serial: {}", explain(wait, short)));
        }
        for gate in &view.gates {
            out.push(format!(
                "  gate {}: {} {}",
                gate.kind,
                short(&gate.id),
                gate.note
            ));
        }
    }
    out
}
