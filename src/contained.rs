use anyhow::Result;
use serde_json::{Value, json};

use crate::discovery::{Scope, Skipped};
use crate::github::Client;
use crate::model::Pr;

const CHUNK: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub slug: String,
    pub reference: String,
    pub sha: String,
}

pub fn build_query(targets: &[Target]) -> (String, Value) {
    let mut declarations = Vec::new();
    let mut selections = Vec::new();
    let mut variables = serde_json::Map::new();
    for (i, target) in targets.iter().enumerate() {
        let (owner, name) = target
            .slug
            .split_once('/')
            .unwrap_or((target.slug.as_str(), ""));
        declarations.push(format!(
            "$o{i}: String!, $r{i}: String!, $q{i}: String!, $h{i}: String!"
        ));
        selections.push(format!(
            "t{i}: repository(owner: $o{i}, name: $r{i}) {{ ref(qualifiedName: $q{i}) {{ compare(headRef: $h{i}) {{ aheadBy behindBy status }} }} }}"
        ));
        variables.insert(format!("o{i}"), json!(owner));
        variables.insert(format!("r{i}"), json!(name));
        variables.insert(
            format!("q{i}"),
            json!(format!("refs/heads/{}", target.reference)),
        );
        variables.insert(format!("h{i}"), json!(target.sha));
    }
    let query = format!(
        "query({}) {{ {} }}",
        declarations.join(", "),
        selections.join(" ")
    );
    (query, Value::Object(variables))
}

pub fn parse(response: &Value, count: usize) -> Vec<bool> {
    (0..count)
        .map(|i| {
            response
                .pointer(&format!("/data/t{i}/ref/compare/aheadBy"))
                .and_then(|v| v.as_u64())
                .is_some_and(|ahead| ahead == 0)
        })
        .collect()
}

pub fn check(client: &mut Client, targets: &[Target]) -> Result<Vec<bool>> {
    let mut verdicts = Vec::with_capacity(targets.len());
    for chunk in targets.chunks(CHUNK) {
        let (query, variables) = build_query(chunk);
        let response = client.graphql("check which PRs are already merged", &query, variables)?;
        verdicts.extend(parse(&response, chunk.len()));
    }
    Ok(verdicts)
}

pub fn refs_for(pr: &Pr, train: &str) -> Vec<String> {
    if pr.head_sha.is_empty() {
        return Vec::new();
    }
    let mut refs = vec![pr.base_ref.clone()];
    if !pr.base_ref.eq_ignore_ascii_case(train) && !pr.head_ref.eq_ignore_ascii_case(train) {
        refs.push(train.to_string());
    }
    refs
}

pub fn targets(ready: &[(Pr, Scope)], train: &str) -> Vec<Target> {
    ready
        .iter()
        .flat_map(|(pr, _)| {
            refs_for(pr, train).into_iter().map(|reference| Target {
                slug: pr.key.slug(),
                reference,
                sha: pr.head_sha.clone(),
            })
        })
        .collect()
}

pub fn apply(
    ready: Vec<(Pr, Scope)>,
    train: &str,
    verdicts: &[bool],
) -> (Vec<(Pr, Scope)>, Vec<Skipped>) {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    let mut cursor = 0;
    for (pr, scope) in ready {
        let refs = refs_for(&pr, train);
        let found = refs
            .iter()
            .enumerate()
            .find(|(offset, _)| verdicts.get(cursor + offset).copied().unwrap_or(false))
            .map(|(_, reference)| reference.clone());
        cursor += refs.len();
        match found {
            Some(reference) => skipped.push(Skipped {
                pr,
                scope,
                reasons: vec![format!("already in {reference}")],
            }),
            None => kept.push((pr, scope)),
        }
    }
    (kept, skipped)
}

pub fn filter(
    client: &mut Client,
    ready: Vec<(Pr, Scope)>,
    train: &str,
) -> Result<(Vec<(Pr, Scope)>, Vec<Skipped>)> {
    let wanted = targets(&ready, train);
    if wanted.is_empty() {
        return Ok((ready, Vec::new()));
    }
    let verdicts = check(client, &wanted)?;
    Ok(apply(ready, train, &verdicts))
}
