use std::collections::{BTreeMap, BTreeSet};

use super::Bundled;
use crate::merge::{Continuation, Kind, Pending, STOPPED, State, Status, StepReport};
use crate::plan::Plan;
use crate::planner::{Item, Rule};

fn downstream(plan: &Plan, id: &str) -> BTreeSet<String> {
    let mut successors: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for item in &plan.items {
        for dependency in &item.after {
            successors.entry(&dependency.id).or_default().push(&item.id);
        }
    }
    let mut found = BTreeSet::new();
    let mut pending = vec![id];
    while let Some(current) = pending.pop() {
        if let Some(next) = successors.get(current) {
            for &id in next {
                if found.insert(id.to_string()) {
                    pending.push(id);
                }
            }
        }
    }
    found.remove(id);
    found
}

pub(super) fn continuation(env: &super::Env, item: &Item) -> Continuation {
    let plan = env.plan;
    let mut dependents = downstream(plan, &item.id);
    let mut has_consumers = has_consumers(plan, &item.id, &dependents);
    for contract in env
        .queued_bumps
        .iter()
        .filter(|contract| contract.publishers.contains(&item.id))
    {
        has_consumers = true;
        dependents.insert(contract.id.clone());
        dependents.extend(downstream(plan, &contract.id));
    }
    Continuation {
        independent: plan
            .items
            .iter()
            .filter(|candidate| candidate.id != item.id && !dependents.contains(&candidate.id))
            .map(|candidate| (candidate.id.clone(), candidate.sha.clone()))
            .collect(),
        has_consumers,
        dependents: dependents.into_iter().collect(),
    }
}

fn has_consumers(plan: &Plan, id: &str, dependents: &BTreeSet<String>) -> bool {
    plan.items
        .iter()
        .filter(|item| dependents.contains(&item.id))
        .any(|item| {
            item.after.iter().any(|dependency| {
                dependency.rule != Rule::Target
                    && (dependency.id == id || dependents.contains(&dependency.id))
            })
        })
}

struct FailedDeploy {
    id: String,
    repo: String,
    base: String,
    continuation: Option<Continuation>,
    dependents: BTreeSet<String>,
    inherited: bool,
    unmerged: bool,
    gate: bool,
}

pub(super) fn isolable(plan: &Plan, step: &StepReport) -> bool {
    step.kind == Kind::Merge
        && step.status == Status::Failed
        && step.merge_sha.is_none()
        && plan.questions.is_empty()
        && plan.items.iter().any(|item| item.id == step.target)
}

#[derive(Default)]
pub(super) struct DeployFailures {
    failures: Vec<FailedDeploy>,
}

pub(super) struct Blocked {
    pub id: String,
    pub detail: String,
}

impl FailedDeploy {
    fn blocked(&self, reason: String) -> Blocked {
        if self.gate {
            return Blocked {
                id: self.id.clone(),
                detail: STOPPED.to_string(),
            };
        }
        let what = if self.unmerged { "merge" } else { "deploy" };
        Blocked {
            id: self.id.clone(),
            detail: format!("blocked by failed {what} of {}: {reason}", self.id),
        }
    }

    fn same_target(&self, repo: &str, base: &str) -> bool {
        self.repo.eq_ignore_ascii_case(repo) && self.base == base
    }

    fn item(&self, item: &Item) -> Option<Blocked> {
        if self.gate && self.id == item.id && !item.gate_late {
            return None;
        }
        if self.dependents.contains(&item.id) || self.id == item.id {
            return Some(self.blocked(format!("{} depends on it", item.id)));
        }
        let repo = format!("{}/{}", item.org, item.repo);
        if self.same_target(&repo, &item.base) {
            return Some(self.blocked(format!("same target {repo}@{}", item.base)));
        }
        let proven = self.inherited
            || self.unmerged
            || self
                .continuation
                .as_ref()
                .and_then(|saved| saved.independent.get(&item.id))
                == Some(&item.sha);
        (!proven).then(|| {
            self.blocked(format!(
                "no saved independence proof for {} at its current head",
                item.id
            ))
        })
    }
}

impl DeployFailures {
    pub fn record(&mut self, pending: &Pending, plan: &Plan, inherited: bool) -> bool {
        let mut dependents = downstream(plan, &pending.id);
        if let Some(saved) = &pending.continuation {
            dependents.extend(saved.dependents.iter().cloned());
        }
        let has_consumers = has_consumers(plan, &pending.id, &dependents)
            || pending
                .continuation
                .as_ref()
                .is_some_and(|saved| saved.has_consumers);
        self.failures.push(FailedDeploy {
            id: pending.id.clone(),
            repo: pending.repo.clone(),
            base: pending.base.clone(),
            continuation: pending.continuation.clone(),
            dependents,
            inherited,
            unmerged: false,
            gate: false,
        });
        has_consumers
    }

    pub fn record_unmerged(&mut self, item: &Item, plan: &Plan) {
        self.failures.push(FailedDeploy {
            id: item.id.clone(),
            repo: format!("{}/{}", item.org, item.repo),
            base: item.base.clone(),
            continuation: None,
            dependents: downstream(plan, &item.id),
            inherited: false,
            unmerged: true,
            gate: false,
        });
    }

    pub fn record_gate(&mut self, item: &Item, plan: &Plan) {
        self.failures.push(FailedDeploy {
            id: item.id.clone(),
            repo: format!("{}/{}", item.org, item.repo),
            base: item.base.clone(),
            continuation: None,
            dependents: downstream(plan, &item.id),
            inherited: false,
            unmerged: true,
            gate: true,
        });
    }

    pub fn holds_gate(&self, id: &str) -> bool {
        self.failures
            .iter()
            .any(|failed| failed.gate && failed.id == id)
    }

    pub fn blocker(
        &self,
        index: usize,
        steps: &[StepReport],
        plan: &Plan,
        bundles: &BTreeMap<String, Bundled>,
        state: &State,
    ) -> Option<Blocked> {
        let step = &steps[index];
        let item = match step.kind {
            Kind::Merge => plan.items.iter().find(|item| item.id == step.target),
            Kind::Wait | Kind::Gate => {
                let next = steps[index + 1..]
                    .iter()
                    .find(|next| next.kind == Kind::Merge)
                    .and_then(|next| plan.items.iter().find(|item| item.id == next.target));
                next.filter(|item| {
                    step.kind == Kind::Wait
                        || (item.gate_late
                            && step.target.starts_with(&format!("{} waits for ", item.id)))
                })
            }
            _ => None,
        };
        for failed in &self.failures {
            if failed.gate
                && step.kind == Kind::Gate
                && item.is_some_and(|item| item.id == failed.id)
            {
                continue;
            }
            if let Some(item) = item {
                if let Some(blocked) = failed.item(item) {
                    return Some(blocked);
                }
            } else if step.kind == Kind::Resume {
                if let Some(pending) = state
                    .pending
                    .iter()
                    .find(|pending| pending.id == step.target)
                {
                    if failed.same_target(&pending.repo, &pending.base) {
                        return Some(
                            failed
                                .blocked(format!("same target {}@{}", pending.repo, pending.base)),
                        );
                    }
                    if failed.dependents.contains(&pending.id) {
                        return Some(failed.blocked(format!("{} depends on it", pending.id)));
                    }
                }
            } else if step.kind == Kind::OpenPr
                && let Some(bundle) = bundles.get(&step.target)
            {
                if failed.same_target(&bundle.slug, &bundle.branch) {
                    return Some(
                        failed.blocked(format!("same target {}@{}", bundle.slug, bundle.branch)),
                    );
                }
                for id in &bundle.ids {
                    let item = plan.items.iter().find(|item| item.id == *id);
                    match item {
                        Some(item) => {
                            if let Some(blocked) = failed.item(item) {
                                return Some(blocked);
                            }
                        }
                        None => {
                            return Some(failed.blocked(format!(
                                "no saved independence proof for bundle member {id}"
                            )));
                        }
                    }
                }
            }
        }
        None
    }
}
