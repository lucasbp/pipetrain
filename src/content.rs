use anyhow::Result;
use serde_json::{Value, json};

use crate::contained::refs_for;
use crate::discovery::{Scope, Skipped};
use crate::github::Client;
use crate::model::Pr;

pub const MAX_LOOKUPS: usize = 50;
pub const MAX_FILES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub slug: String,
    pub reference: String,
    pub sha: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub slug: String,
    pub expression: String,
}

pub type Reading = Option<Option<String>>;

pub fn probes(checks: &[Check]) -> Vec<Probe> {
    let mut out = Vec::new();
    for check in checks {
        out.push(Probe {
            slug: check.slug.clone(),
            expression: check.reference.clone(),
        });
        for path in &check.paths {
            out.push(Probe {
                slug: check.slug.clone(),
                expression: format!("{}:{path}", check.reference),
            });
            out.push(Probe {
                slug: check.slug.clone(),
                expression: format!("{}:{path}", check.sha),
            });
        }
    }
    out
}

pub fn build_query(probes: &[Probe]) -> (String, Value) {
    let mut declarations = Vec::new();
    let mut selections = Vec::new();
    let mut variables = serde_json::Map::new();
    for (i, probe) in probes.iter().enumerate() {
        let (owner, name) = probe
            .slug
            .split_once('/')
            .unwrap_or((probe.slug.as_str(), ""));
        declarations.push(format!("$o{i}: String!, $r{i}: String!, $e{i}: String!"));
        selections.push(format!(
            "p{i}: repository(owner: $o{i}, name: $r{i}) {{ object(expression: $e{i}) {{ oid }} }}"
        ));
        variables.insert(format!("o{i}"), json!(owner));
        variables.insert(format!("r{i}"), json!(name));
        variables.insert(format!("e{i}"), json!(probe.expression));
    }
    let query = format!(
        "query({}) {{ {} }}",
        declarations.join(", "),
        selections.join(" ")
    );
    (query, Value::Object(variables))
}

pub fn parse(response: &Value, count: usize) -> Vec<Reading> {
    (0..count)
        .map(|i| match response.pointer(&format!("/data/p{i}/object")) {
            Some(Value::Null) => Some(None),
            Some(object) => object
                .get("oid")
                .and_then(Value::as_str)
                .map(|oid| Some(oid.to_string())),
            None => None,
        })
        .collect()
}

fn same(readings: &[Reading]) -> bool {
    let Some((guard, pairs)) = readings.split_first() else {
        return false;
    };
    matches!(guard, Some(Some(_)))
        && !pairs.is_empty()
        && pairs.len() % 2 == 0
        && pairs
            .chunks(2)
            .all(|pair| matches!((&pair[0], &pair[1]), (Some(a), Some(b)) if a == b))
}

pub fn verdicts(checks: &[Check], readings: &[Reading]) -> Vec<bool> {
    let mut cursor = 0;
    checks
        .iter()
        .map(|check| {
            let width = 1 + 2 * check.paths.len();
            let slice = readings.get(cursor..cursor + width);
            cursor += width;
            slice.is_some_and(same)
        })
        .collect()
}

pub fn check(client: &mut Client, checks: &[Check]) -> Result<Vec<bool>> {
    let lookups = probes(checks);
    let mut readings = Vec::with_capacity(lookups.len());
    for chunk in lookups.chunks(MAX_LOOKUPS) {
        let (query, variables) = build_query(chunk);
        let response =
            client.graphql("compare PR file content with the base", &query, variables)?;
        readings.extend(parse(&response, chunk.len()));
    }
    Ok(verdicts(checks, &readings))
}

pub fn eligible(pr: &Pr) -> bool {
    pr.files_complete && !pr.head_sha.is_empty() && (1..=MAX_FILES).contains(&pr.files.len())
}

pub fn checks(ready: &[(Pr, Scope)], train: &str) -> Vec<Check> {
    ready
        .iter()
        .filter(|(pr, _)| eligible(pr))
        .flat_map(|(pr, _)| {
            refs_for(pr, train).into_iter().map(move |reference| Check {
                slug: pr.key.slug(),
                reference,
                sha: pr.head_sha.clone(),
                paths: pr.files.iter().map(|file| file.path.clone()).collect(),
            })
        })
        .collect()
}

type Filtered = (Vec<(Pr, Scope)>, Vec<Skipped>);

pub fn apply(ready: Vec<(Pr, Scope)>, train: &str, verdicts: &[bool]) -> Filtered {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    let mut cursor = 0;
    for (pr, scope) in ready {
        let refs = if eligible(&pr) {
            refs_for(&pr, train)
        } else {
            Vec::new()
        };
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
                reasons: vec![format!("already in {reference} (same content)")],
            }),
            None => kept.push((pr, scope)),
        }
    }
    (kept, skipped)
}

pub fn filter(client: &mut Client, ready: Vec<(Pr, Scope)>, train: &str) -> Result<Filtered> {
    let wanted = checks(&ready, train);
    if wanted.is_empty() {
        return Ok((ready, Vec::new()));
    }
    let verdicts = check(client, &wanted)?;
    Ok(apply(ready, train, &verdicts))
}
