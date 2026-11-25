use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use anyhow::Result;
use chrono::NaiveDate;

use crate::bundle::is_train_branch;
use crate::config::{Config, DEFAULT_TRAIN_DATE_FORMAT, render_date};
use crate::model::{Pr, PrState, Question};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Train,
    SharedBranch,
    IntoTrain,
    Explicit,
    Released,
    Author,
    StackParent,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Train => "train",
            Scope::SharedBranch => "shared-branch",
            Scope::IntoTrain => "into-train",
            Scope::Explicit => "explicit",
            Scope::Released => "released",
            Scope::Author => "author",
            Scope::StackParent => "stack-parent",
        }
    }

    pub fn from_name(name: &str) -> Option<Scope> {
        [
            Scope::Train,
            Scope::SharedBranch,
            Scope::IntoTrain,
            Scope::Explicit,
            Scope::Released,
            Scope::Author,
            Scope::StackParent,
        ]
        .into_iter()
        .find(|scope| scope.as_str() == name)
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

fn labelled(pr: &Pr, cfg: &Config) -> bool {
    cfg.train_label
        .as_deref()
        .is_some_and(|label| pr.has_label(label))
}

fn shared_branches(all: &[Pr], cfg: &Config, train: &str) -> HashSet<String> {
    let mut repos: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for pr in all {
        let head = pr.head_ref.as_str();
        let ignored = labelled(pr, cfg)
            || head.eq_ignore_ascii_case(train)
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

fn is_author(pr: &Pr, authors: &[String]) -> bool {
    !pr.is_draft && authors.iter().any(|a| a.eq_ignore_ascii_case(&pr.author))
}

fn classify_scope(
    pr: &Pr,
    cfg: &Config,
    shared: &HashSet<String>,
    train: &str,
    authors: &[String],
) -> Option<Scope> {
    if is_train_branch(&pr.head_ref, train) || labelled(pr, cfg) {
        Some(Scope::Train)
    } else if shared.contains(&pr.head_ref) {
        Some(Scope::SharedBranch)
    } else if is_train_branch(&pr.base_ref, train) {
        Some(Scope::IntoTrain)
    } else if is_author(pr, authors) {
        Some(Scope::Author)
    } else {
        None
    }
}

fn sibling_questions(out: &Discovered) -> Vec<Question> {
    let mut questions = Vec::new();
    for (pr, scope) in &out.ready {
        if *scope != Scope::SharedBranch {
            continue;
        }
        for blocked in out
            .skipped
            .iter()
            .filter(|s| s.scope == Scope::SharedBranch && s.pr.head_ref == pr.head_ref)
        {
            questions.push(Question::new(
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
    questions
}

fn stacked_questions(out: &Discovered, train: &str) -> Vec<Question> {
    let mut questions = Vec::new();
    for (pr, scope) in &out.ready {
        if *scope != Scope::Train {
            continue;
        }
        for blocked in out.skipped.iter().filter(|s| {
            s.scope != Scope::Train
                && s.pr.key.slug().eq_ignore_ascii_case(&pr.key.slug())
                && s.pr.base_ref.eq_ignore_ascii_case(train)
        }) {
            questions.push(Question::new(
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
    questions
}

pub(crate) fn refresh_questions(out: &mut Discovered, train: &str) {
    out.questions = sibling_questions(out);
    out.questions.extend(stacked_questions(out, train));
}

pub fn discover(all: Vec<Pr>, cfg: &Config, train: &str) -> Discovered {
    discover_authored(all, cfg, train, &[])
}

pub fn discover_authored(
    all: Vec<Pr>,
    cfg: &Config,
    train: &str,
    authors: &[String],
) -> Discovered {
    let all = unique(all);
    let shared = shared_branches(&all, cfg, train);
    let mut out = Discovered::default();
    for pr in all {
        let Some(scope) = classify_scope(&pr, cfg, &shared, train, authors) else {
            continue;
        };
        let reasons = cfg.blockers(&pr);
        if reasons.is_empty() {
            out.ready.push((pr, scope));
        } else {
            out.skipped.push(Skipped { pr, scope, reasons });
        }
    }
    let siblings = sibling_questions(&out);
    out.questions.extend(siblings);
    let stacked = stacked_questions(&out, train);
    out.questions.extend(stacked);
    out
}

pub fn unique(prs: Vec<Pr>) -> Vec<Pr> {
    let mut seen = HashSet::new();
    prs.into_iter()
        .filter(|pr| seen.insert(pr.key.lookup_id()))
        .collect()
}

pub fn released(all: Vec<Pr>, cfg: &Config, train: &str) -> Discovered {
    let mut out = Discovered::default();
    for pr in unique(all) {
        let scope = if is_train_branch(&pr.head_ref, train) || labelled(&pr, cfg) {
            Scope::Train
        } else if is_train_branch(&pr.base_ref, train) {
            Scope::IntoTrain
        } else {
            Scope::Released
        };
        let reasons = cfg.blockers(&pr);
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
    for pr in unique(prs) {
        let reasons = cfg.blockers(&pr);
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

pub fn add_authored(out: &mut Discovered, pool: &[Pr], cfg: &Config, authors: &[String]) {
    let mut seen: HashSet<String> = out
        .ready
        .iter()
        .map(|(pr, _)| pr.key.lookup_id())
        .chain(out.skipped.iter().map(|s| s.pr.key.lookup_id()))
        .collect();
    for pr in pool {
        if !is_author(pr, authors) || !seen.insert(pr.key.lookup_id()) {
            continue;
        }
        push_classified(out, pr.clone(), Scope::Author, cfg);
    }
}

fn push_classified(out: &mut Discovered, pr: Pr, scope: Scope, cfg: &Config) {
    let reasons = cfg.blockers(&pr);
    if reasons.is_empty() {
        out.ready.push((pr, scope));
    } else {
        out.skipped.push(Skipped { pr, scope, reasons });
    }
}

fn lands_on_a_branch_of_its_own(pr: &Pr, cfg: &Config) -> bool {
    let base = pr.base_ref.to_lowercase();
    !base.is_empty()
        && !base.eq_ignore_ascii_case(&pr.default_branch)
        && !PLAIN_BRANCHES.contains(&base.as_str())
        && !base.starts_with(&cfg.train_prefix.to_lowercase())
}

fn same_repo(a: &Pr, b: &Pr) -> bool {
    a.key.slug().eq_ignore_ascii_case(&b.key.slug())
}

pub fn add_stack_parents(
    out: &mut Discovered,
    cfg: &Config,
    mut find: impl FnMut(&Pr) -> Result<Vec<Pr>>,
) -> Result<()> {
    let mut seen: HashSet<String> = out
        .ready
        .iter()
        .map(|(pr, _)| pr.key.lookup_id())
        .chain(out.skipped.iter().map(|s| s.pr.key.lookup_id()))
        .collect();
    let mut queue: VecDeque<Pr> = out
        .ready
        .iter()
        .map(|(pr, _)| pr.clone())
        .chain(out.skipped.iter().map(|s| s.pr.clone()))
        .collect();
    while let Some(child) = queue.pop_front() {
        if !lands_on_a_branch_of_its_own(&child, cfg) {
            continue;
        }
        let parents: Vec<Pr> = find(&child)?
            .into_iter()
            .filter(|p| {
                p.key != child.key
                    && same_repo(p, &child)
                    && p.head_ref == child.base_ref
                    && p.state == PrState::Open
            })
            .collect();
        let [parent] = parents.as_slice() else {
            continue;
        };
        if !seen.insert(parent.key.lookup_id()) {
            continue;
        }
        push_classified(out, parent.clone(), Scope::StackParent, cfg);
        queue.push_back(parent.clone());
    }
    hold_children_of_unready_parents(out);
    Ok(())
}

fn hold_children_of_unready_parents(out: &mut Discovered) {
    loop {
        let held = out.ready.iter().position(|(child, _)| {
            out.skipped.iter().any(|s| {
                s.pr.key != child.key && same_repo(&s.pr, child) && s.pr.head_ref == child.base_ref
            })
        });
        let Some(index) = held else {
            return;
        };
        let (pr, scope) = out.ready.remove(index);
        let parent = out
            .skipped
            .iter()
            .find(|s| s.pr.key != pr.key && same_repo(&s.pr, &pr) && s.pr.head_ref == pr.base_ref)
            .map(|s| s.pr.key.to_string())
            .unwrap_or_default();
        out.skipped.push(Skipped {
            pr,
            scope,
            reasons: vec![format!("stack parent {parent} not ready")],
        });
    }
}
