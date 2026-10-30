use std::collections::BTreeMap;

use serde::Serialize;

use crate::bundle::{self, Bundle};
use crate::config::Rules;
use crate::discovery::Skipped;
use crate::model::{Pr, Question};
use crate::planner::{self, DepState, Item, PlanInput};
use crate::profile::ProfileSource;
use crate::publishers::{Publishers, References};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Serialize)]
pub struct SkippedOut {
    pub id: String,
    pub title: String,
    pub url: String,
    pub scope: String,
    pub head: String,
    pub reasons: Vec<String>,
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
    pub items: Vec<Item>,
    pub gate: Vec<String>,
    pub lanes: Vec<Vec<String>>,
    pub bundles: Vec<Bundle>,
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
            scope: s.scope.as_str().to_string(),
            head: s.pr.head_ref,
            reasons: s.reasons,
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

pub fn build(rules: &Rules, gathered: Gathered) -> Plan {
    let planned = planner::plan(PlanInput {
        rules,
        prs: gathered.ready,
        external: gathered.external,
        publishers: gathered.publishers,
        references: gathered.references,
    });
    let questions = merged(gathered.questions, planned.questions);
    let skipped = skipped_out(gathered.skipped);
    let bundles = bundle::suggest(
        &planned.items,
        &gathered.deploy_branch,
        &gathered.train_prefix,
    );
    let (deploys_saved, deploys_now) =
        deploy_counts(&planned.items, &bundles, &gathered.train_prefix);
    Plan {
        schema: SCHEMA,
        generated_at: gathered.generated_at,
        train: gathered.train,
        deploy_branch: gathered.deploy_branch,
        profile: None,
        orgs: gathered.orgs,
        items: planned.items,
        gate: planned.gate,
        lanes: planned.lanes,
        bundles,
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
        },
    }
}
