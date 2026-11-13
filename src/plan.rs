use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::bundle::{self, Bundle};
use crate::config::Rules;
use crate::conflicts::{self, Conflict, Snapshot};
use crate::discovery::Skipped;
use crate::model::{Pr, Question, Readiness};
use crate::planner::{self, DepState, Item, PlanInput, Rule};
use crate::profile::ProfileSource;
use crate::publishers::{Publishers, References};

pub const SCHEMA: u32 = 1;

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Serialize)]
pub struct SkippedOut {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    pub scope: String,
    pub head: String,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub requests: u32,
    pub rate_remaining: Option<u64>,
    pub peak_parallel: usize,
    pub max_parallel: usize,
    pub deploys_saved: usize,
    pub deploys_now: usize,
    pub deploys_after: usize,
    pub cache_hits: u32,
    pub cache_misses: u32,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub hidden_by_author: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub feed_checks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileInfo {
    pub name: String,
    pub source: ProfileSource,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub schema: u32,
    pub generated_at: String,
    pub train: String,
    pub deploy_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileInfo>,
    pub orgs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shown_authors: Option<Vec<String>>,
    pub items: Vec<Item>,
    pub preflight: Vec<Readiness>,
    pub gate: Vec<String>,
    pub lanes: Vec<Vec<String>>,
    pub bundles: Vec<Bundle>,
    pub conflicts: Vec<Conflict>,
    pub skipped: Vec<SkippedOut>,
    pub questions: Vec<Question>,
    pub notes: Vec<String>,
    pub stats: Stats,
}

pub struct Gathered {
    pub generated_at: String,
    pub train: String,
    pub deploy_branch: String,
    pub train_prefix: String,
    pub orgs: Vec<String>,
    pub ready: Vec<(Pr, String)>,
    pub skipped: Vec<Skipped>,
    pub questions: Vec<Question>,
    pub external: BTreeMap<String, DepState>,
    pub publishers: Publishers,
    pub references: References,
    pub max_parallel: usize,
    pub requests: u32,
    pub rate_remaining: Option<u64>,
}

fn skipped_out(skipped: Vec<Skipped>) -> Vec<SkippedOut> {
    let mut out: Vec<SkippedOut> = skipped
        .into_iter()
        .map(|s| SkippedOut {
            id: s.pr.key.to_string(),
            title: s.pr.title,
            url: s.pr.url,
            author: s.pr.author,
            scope: s.scope.as_str().to_string(),
            head: s.pr.head_ref,
            reasons: s.reasons,
            hidden: false,
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn merged(mut first: Vec<Question>, second: Vec<Question>) -> Vec<Question> {
    first.extend(second);
    first
}

fn deploy_counts(items: &[Item], bundles: &[Bundle], train_prefix: &str) -> (usize, usize) {
    let saved: usize = bundles.iter().map(|b| b.saved).sum();
    let now = items
        .iter()
        .filter(|i| !bundle::is_stacked(&i.base, train_prefix))
        .count();
    (saved, now)
}

fn snapshots(ready: &[(Pr, String)]) -> Vec<Snapshot> {
    ready
        .iter()
        .map(|(pr, _)| Snapshot {
            id: pr.key.to_string(),
            repo: pr.key.slug().to_lowercase(),
            paths: pr.files.iter().map(|f| f.path.clone()).collect(),
        })
        .collect()
}

fn stacked_pairs(items: &[Item]) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for item in items {
        for dep in item.after.iter().filter(|d| d.rule == Rule::Stacked) {
            pairs.insert((item.id.clone(), dep.id.clone()));
            pairs.insert((dep.id.clone(), item.id.clone()));
        }
    }
    pairs
}

pub fn build(rules: &Rules, gathered: Gathered) -> Plan {
    let snapshots = snapshots(&gathered.ready);
    let selected: Vec<&Pr> = gathered
        .ready
        .iter()
        .map(|(pr, _)| pr)
        .chain(gathered.skipped.iter().map(|skipped| &skipped.pr))
        .collect();
    let mut preflight: Vec<Readiness> = selected.iter().map(|pr| {
        let reasons = gathered.skipped.iter().find(|skipped| skipped.pr.key == pr.key)
            .map(|skipped| skipped.reasons.clone()).unwrap_or_default();
        let mut readiness = pr.readiness(reasons);
        if !pr.default_branch.is_empty() && pr.base_ref != pr.default_branch
            && !bundle::is_stacked(&pr.base_ref, &gathered.train_prefix)
            && !selected.iter().any(|parent| parent.key != pr.key
                && parent.key.slug().eq_ignore_ascii_case(&pr.key.slug()) && parent.head_ref == pr.base_ref)
        {
            readiness.warnings.push(format!(
                "base not landed: base `{}` is not default `{}` or a train branch and no selected parent has that head; select the parent PR or verify the intended destination",
                pr.base_ref, pr.default_branch
            ));
        }
        readiness
    }).collect();
    let planned = planner::plan(PlanInput {
        rules,
        prs: gathered.ready,
        external: gathered.external,
        publishers: gathered.publishers,
        references: gathered.references,
    });
    let questions = merged(gathered.questions, planned.questions);
    let skipped = skipped_out(gathered.skipped);
    for readiness in &mut preflight {
        if readiness.status == "blocked" {
            continue;
        }
        if let Some(item) = planned.items.iter().find(|item| item.id == readiness.id) {
            let predecessors: Vec<String> = item
                .after
                .iter()
                .filter(|dep| planned.items.iter().any(|prior| prior.id == dep.id))
                .map(|dep| format!("{}: {}", dep.id, dep.reason))
                .collect();
            let unresolved: Vec<String> = questions
                .iter()
                .filter(|question| {
                    question.pr.is_none() || question.pr.as_deref() == Some(&readiness.id)
                })
                .map(|question| question.message.clone())
                .collect();
            if !predecessors.is_empty() || !unresolved.is_empty() || item.manual_pause {
                readiness.status = "waiting".to_string();
                readiness.causes = predecessors;
                readiness.causes.extend(unresolved);
                if item.manual_pause {
                    readiness
                        .causes
                        .push("manual gate requires confirmation".to_string());
                }
                readiness.next_step = "complete the earlier steps and resolve questions or confirm the gate, then revalidate this PR".to_string();
            }
        }
    }
    preflight.sort_by(|a, b| a.id.cmp(&b.id));
    let bundles = bundle::suggest(
        &planned.items,
        &gathered.deploy_branch,
        &gathered.train_prefix,
    );
    let conflicts = conflicts::find(&snapshots, &stacked_pairs(&planned.items));
    let (deploys_saved, deploys_now) =
        deploy_counts(&planned.items, &bundles, &gathered.train_prefix);
    Plan {
        schema: SCHEMA,
        generated_at: gathered.generated_at,
        train: gathered.train,
        deploy_branch: gathered.deploy_branch,
        profile: None,
        orgs: gathered.orgs,
        shown_authors: None,
        items: planned.items,
        preflight,
        gate: planned.gate,
        lanes: planned.lanes,
        bundles,
        conflicts,
        skipped,
        questions,
        notes: planned.notes,
        stats: Stats {
            requests: gathered.requests,
            rate_remaining: gathered.rate_remaining,
            peak_parallel: planned.peak_parallel,
            max_parallel: gathered.max_parallel,
            deploys_saved,
            deploys_now,
            deploys_after: deploys_now - deploys_saved,
            cache_hits: 0,
            cache_misses: 0,
            elapsed_ms: 0,
            hidden_by_author: 0,
            feed_checks: 0,
        },
    }
}

impl Plan {
    pub fn has_eligible(&self) -> bool {
        self.preflight
            .iter()
            .any(|pr| pr.status == "eligible" && !self.is_hidden(&pr.id))
    }

    pub fn preflight_lines(&self) -> Vec<String> {
        self.preflight
            .iter()
            .filter(|pr| !self.is_hidden(&pr.id))
            .flat_map(Readiness::lines)
            .collect()
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.items.iter().any(|i| i.id == id && i.hidden)
            || self.skipped.iter().any(|s| s.id == id && s.hidden)
    }
}

pub fn hide_other_authors(plan: &mut Plan, logins: &[String]) {
    let wanted: BTreeSet<String> = logins.iter().map(|l| l.to_lowercase()).collect();
    let is_hidden = |author: &str| !author.is_empty() && !wanted.contains(&author.to_lowercase());
    let mut hidden_ids = BTreeSet::new();
    for item in &mut plan.items {
        item.hidden = is_hidden(&item.author);
        if item.hidden {
            hidden_ids.insert(item.id.clone());
        }
    }
    for skipped in &mut plan.skipped {
        skipped.hidden = is_hidden(&skipped.author);
        if skipped.hidden {
            hidden_ids.insert(skipped.id.clone());
        }
    }
    for question in &mut plan.questions {
        question.hidden = question
            .pr
            .as_ref()
            .is_some_and(|pr| hidden_ids.contains(pr));
    }
    plan.stats.hidden_by_author = hidden_ids.len();
    plan.shown_authors = Some(wanted.into_iter().collect());
}
