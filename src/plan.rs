use std::collections::BTreeMap;

use serde::Serialize;

use crate::bundle::{self, Bundle};
use crate::config::Rules;
use crate::discovery::Skipped;
use crate::model::{Pr, Question};
use crate::planner::{self, DepState, Item, PlanInput};
use crate::publishers::Publishers;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Serialize)]
pub struct SkippedOut {
    pub id: String,
    pub title: String,
    pub url: String,
    pub scope: String,
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

#[derive(Debug, Serialize)]
pub struct Plan {
    pub schema: u32,
    pub generated_at: String,
    pub train: String,
    pub deploy_branch: String,
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
    pub max_parallel: usize,
    pub requests: u32,
    pub rate_remaining: Option<u64>,
}

pub fn build(rules: &Rules, gathered: Gathered) -> Plan {
    let planned = planner::plan(PlanInput {
        rules,
        prs: gathered.ready,
        external: gathered.external,
        publishers: gathered.publishers,
    });
    let mut questions = gathered.questions;
    questions.extend(planned.questions);
    let mut skipped: Vec<SkippedOut> = gathered
        .skipped
        .into_iter()
        .map(|s| SkippedOut {
            id: s.pr.key.to_string(),
            title: s.pr.title,
            url: s.pr.url,
            scope: s.scope.as_str().to_string(),
            reasons: s.reasons,
        })
        .collect();
    skipped.sort_by(|a, b| a.id.cmp(&b.id));
    let bundles = bundle::suggest(
        &planned.items,
        &gathered.deploy_branch,
        &gathered.train_prefix,
    );
    let deploys_saved: usize = bundles.iter().map(|b| b.saved).sum();
    let deploys_now = planned
        .items
        .iter()
        .filter(|i| !bundle::is_stacked(&i.base, &gathered.train_prefix))
        .count();
    Plan {
        schema: SCHEMA,
        generated_at: gathered.generated_at,
        train: gathered.train,
        deploy_branch: gathered.deploy_branch,
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
