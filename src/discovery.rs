use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::NaiveDate;

use crate::config::{Config, DEFAULT_TRAIN_DATE_FORMAT, render_date};
use crate::model::{Pr, Question, Review};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Train,
    SharedBranch,
    IntoTrain,
    Explicit,
    Released,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Train => "train",
            Scope::SharedBranch => "shared-branch",
            Scope::IntoTrain => "into-train",
            Scope::Explicit => "explicit",
            Scope::Released => "released",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub pr: Pr,
    pub scope: Scope,
    pub reasons: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Discovered {
    pub ready: Vec<(Pr, Scope)>,
    pub skipped: Vec<Skipped>,
    pub questions: Vec<Question>,
}

const PLAIN_BRANCHES: [&str; 4] = ["main", "master", "develop", "development"];

pub fn train_branch(cfg: &Config, date: NaiveDate) -> String {
    let formatted = render_date(&cfg.train_date_format, date)
        .or_else(|| render_date(DEFAULT_TRAIN_DATE_FORMAT, date))
        .unwrap_or_default();
    format!("{}{}", cfg.train_prefix, formatted)
}

fn shared_branches(all: &[Pr], cfg: &Config, train: &str) -> HashSet<String> {
    let mut repos: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for pr in all {
        let head = pr.head_ref.as_str();
        let ignored = head.eq_ignore_ascii_case(train)
            || head
                .to_lowercase()
                .starts_with(&cfg.train_prefix.to_lowercase())
            || cfg
                .ignore_branch_prefixes
                .iter()
                .any(|p| head.to_lowercase().starts_with(&p.to_lowercase()))
            || PLAIN_BRANCHES.contains(&head.to_lowercase().as_str());
        if ignored {
            continue;
        }
        repos
            .entry(head)
            .or_default()
            .insert(pr.key.slug().to_lowercase());
    }
    repos
        .into_iter()
        .filter(|(_, slugs)| slugs.len() >= 2)
        .map(|(head, _)| head.to_string())
        .collect()
}

pub fn discover(all: Vec<Pr>, cfg: &Config, train: &str) -> Discovered {
    let shared = shared_branches(&all, cfg, train);
    let mut out = Discovered::default();
    for pr in all {
        let scope = if pr.head_ref.eq_ignore_ascii_case(train) {
            Scope::Train
        } else if shared.contains(&pr.head_ref) {
            Scope::SharedBranch
        } else if pr.base_ref.eq_ignore_ascii_case(train) {
            Scope::IntoTrain
        } else {
            continue;
        };
        let reasons = pr.blockers(cfg.allows_missing_checks(&pr.key.slug()));
        if reasons.is_empty() {
            out.ready.push((pr, scope));
        } else {
            out.skipped.push(Skipped { pr, scope, reasons });
        }
    }
    for (pr, scope) in &out.ready {
        if *scope != Scope::SharedBranch {
            continue;
        }
        for blocked in out
            .skipped
            .iter()
            .filter(|s| s.scope == Scope::SharedBranch && s.pr.head_ref == pr.head_ref)
        {
            out.questions.push(Question::new(
                Some(&pr.key),
                "sibling-not-ready",
                format!(
                    "branch `{}` is also open in {}, which is not ready ({})",
                    pr.head_ref,
                    blocked.pr.key,
                    blocked.reasons.join(", ")
                ),
                "hold this PR until its sibling is ready, or drop the sibling from the change",
            ));
        }
    }
    for (pr, scope) in &out.ready {
        if *scope != Scope::Train {
            continue;
        }
        for blocked in out.skipped.iter().filter(|s| {
            s.scope != Scope::Train
                && s.pr.key.slug().eq_ignore_ascii_case(&pr.key.slug())
                && s.pr.base_ref.eq_ignore_ascii_case(train)
        }) {
            out.questions.push(Question::new(
                Some(&pr.key),
                "stacked-not-ready",
                format!(
                    "{} merges into `{}` and is not ready ({}), so its changes would miss this PR",
                    blocked.pr.key,
                    train,
                    blocked.reasons.join(", ")
                ),
                "get that PR ready before the train, or drop it from the deploy",
            ));
        }
    }
    out
}

pub fn released(all: Vec<Pr>, cfg: &Config, train: &str) -> Discovered {
    let mut out = Discovered::default();
    for pr in all {
        if pr.review != Review::Approved {
            continue;
        }
        let scope = if pr.head_ref.eq_ignore_ascii_case(train) {
            Scope::Train
        } else if pr.base_ref.eq_ignore_ascii_case(train) {
            Scope::IntoTrain
        } else {
            Scope::Released
        };
        let reasons = pr.blockers(cfg.allows_missing_checks(&pr.key.slug()));
        if reasons.is_empty() {
            out.ready.push((pr, scope));
        } else {
            out.skipped.push(Skipped { pr, scope, reasons });
        }
    }
    out
}

pub fn explicit(prs: Vec<Pr>, cfg: &Config) -> Discovered {
    let mut out = Discovered::default();
    for pr in prs {
        let reasons = pr.blockers(cfg.allows_missing_checks(&pr.key.slug()));
        if reasons.is_empty() {
            out.ready.push((pr, Scope::Explicit));
        } else {
            out.skipped.push(Skipped {
                pr,
                scope: Scope::Explicit,
                reasons,
            });
        }
    }
    out
}
