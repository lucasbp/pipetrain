use std::collections::{BTreeMap, BTreeSet};

use crate::merge::{Kind, StepReport, Summary};

pub(crate) struct MergeProgress {
    total: usize,
    active: BTreeMap<usize, (String, Kind, String)>,
    finished: BTreeMap<usize, StepReport>,
    merged: BTreeSet<usize>,
}

impl MergeProgress {
    pub(crate) fn new(total: usize) -> Self {
        Self {
            total,
            active: BTreeMap::new(),
            finished: BTreeMap::new(),
            merged: BTreeSet::new(),
        }
    }

    pub(crate) fn start(&mut self, index: usize, target: String, kind: Kind) {
        self.active
            .entry(index)
            .or_insert((target, kind, "starting".into()));
    }

    pub(crate) fn update(&mut self, index: usize, message: &str) {
        if let Some((_, _, phase)) = self.active.get_mut(&index) {
            *phase = message.to_string();
        }
    }

    pub(crate) fn merged(&mut self, index: usize) {
        self.merged.insert(index);
    }

    pub(crate) fn finish(&mut self, index: usize, step: &StepReport) {
        if step.kind == Kind::Merge && step.merge_sha.is_some() {
            self.merged(index);
        }
        self.active.remove(&index);
        self.finished.insert(index, step.clone());
    }

    pub(crate) fn frame(&self) -> String {
        let summary = Summary::from_steps(self.finished.values());
        let waiting = self
            .total
            .saturating_sub(self.finished.len() + self.active.len());
        let mut text = format!(
            "waiting: {waiting}, running: {}, merged: {}, deploy confirmed: {}, failed/stopped: {}",
            self.active.len(),
            self.merged.len(),
            summary.deploy_confirmed,
            summary.failed + summary.stopped,
        );
        for (target, kind, message) in self.active.values() {
            let phase = match kind {
                Kind::Gate => "gate",
                Kind::Wait => "gate",
                Kind::OpenPr => "opening deploy PR",
                Kind::Bump => "resolving",
                _ if message.starts_with("checking later deploy") => "resolving",
                _ if message.starts_with("deploy ")
                    || message.starts_with("checking deploy")
                    || message.starts_with("waiting for deploy:") =>
                {
                    "waiting for deploy"
                }
                _ if message.starts_with("merging ") => "merging",
                _ => "readiness",
            };
            if message.starts_with("waiting for deploy:") {
                text.push_str(&format!("\n{target} | {message}"));
            } else {
                text.push_str(&format!("\n{target} | {phase}: {message}"));
            }
        }
        text
    }
}
