use serde_json::Value;

use crate::detect;
use crate::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};

fn text_field(node: &Value, field: &str) -> String {
    node.get(field)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn parse_state(node: &Value) -> PrState {
    match node.get("state").and_then(|v| v.as_str()) {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ => PrState::Open,
    }
}

fn review_states(node: &Value) -> Vec<&str> {
    node.pointer("/latestOpinionatedReviews/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("state").and_then(|s| s.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_review(node: &Value) -> Review {
    match node.get("reviewDecision").and_then(|v| v.as_str()) {
        Some("APPROVED") => Review::Approved,
        Some("CHANGES_REQUESTED") => Review::ChangesRequested,
        Some("REVIEW_REQUIRED") => Review::ReviewRequired,
        _ => review_from_latest(&review_states(node)),
    }
}

fn review_from_latest(reviews: &[&str]) -> Review {
    if reviews.contains(&"CHANGES_REQUESTED") {
        Review::ChangesRequested
    } else if reviews.contains(&"APPROVED") {
        Review::Approved
    } else {
        Review::None
    }
}

fn parse_checks(node: &Value) -> Checks {
    match node
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(|v| v.as_str())
    {
        Some("SUCCESS") => Checks::Success,
        Some("FAILURE") | Some("ERROR") => Checks::Failure,
        Some(_) => Checks::Pending,
        None => Checks::Missing,
    }
}

pub fn parse_pr(node: &Value) -> Option<Pr> {
    let number = node.get("number")?.as_u64()?;
    let repo = node.get("repository")?;
    let org = repo.get("owner")?.get("login")?.as_str()?;
    let name = repo.get("name")?.as_str()?;
    let mergeable = node.get("mergeable").and_then(|v| v.as_str());
    Some(Pr {
        key: PrKey::new(org, name, number),
        state: parse_state(node),
        title: text_field(node, "title"),
        url: text_field(node, "url"),
        body: text_field(node, "body"),
        head_ref: text_field(node, "headRefName"),
        base_ref: text_field(node, "baseRefName"),
        head_sha: text_field(node, "headRefOid"),
        is_draft: node
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        review: parse_review(node),
        checks: parse_checks(node),
        conflicting: mergeable == Some("CONFLICTING"),
        mergeable_unknown: matches!(mergeable, None | Some("UNKNOWN")),
        files: Vec::new(),
        files_complete: true,
    })
}

pub fn parse_search(response: &Value) -> Vec<Pr> {
    response
        .pointer("/data/search/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| nodes.iter().filter_map(parse_pr).collect())
        .unwrap_or_default()
}

pub fn parse_files(value: &Value) -> Vec<ChangedFile> {
    value
        .as_array()
        .map(|files| files.iter().filter_map(parse_file).collect())
        .unwrap_or_default()
}

fn parse_file(f: &Value) -> Option<ChangedFile> {
    let path = f.get("filename")?.as_str()?.to_string();
    let patch = if detect::keeps_patch(&path) {
        f.get("patch").and_then(|p| p.as_str()).map(str::to_string)
    } else {
        None
    };
    Some(ChangedFile { path, patch })
}
