use std::collections::BTreeSet;

use serde_json::Value;

use crate::detect;
use crate::model::{ChangedFile, CheckDetail, Checks, Pr, PrKey, PrState, Review};

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

fn failed_check_name(context: &Value) -> Option<String> {
    match context.get("__typename").and_then(|v| v.as_str()) {
        Some("CheckRun") => match context.get("conclusion").and_then(|v| v.as_str()) {
            Some("FAILURE") | Some("TIMED_OUT") | Some("STARTUP_FAILURE") => {
                Some(context.get("name")?.as_str()?.to_string())
            }
            _ => None,
        },
        Some("StatusContext") => match context.get("state").and_then(|v| v.as_str()) {
            Some("FAILURE") | Some("ERROR") => Some(context.get("context")?.as_str()?.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn parse_failed_checks(node: &Value) -> Vec<String> {
    let names: BTreeSet<String> = node
        .pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| nodes.iter().filter_map(failed_check_name).collect())
        .unwrap_or_default();
    names.into_iter().collect()
}

fn parse_check_details(node: &Value) -> Vec<CheckDetail> {
    let details: BTreeSet<CheckDetail> = node
        .pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|context| {
                    let run = context.get("__typename")?.as_str()? == "CheckRun";
                    let name = context
                        .get(if run { "name" } else { "context" })?
                        .as_str()?
                        .to_string();
                    let state = context
                        .get(if run { "conclusion" } else { "state" })
                        .and_then(Value::as_str)
                        .or_else(|| context.get("status").and_then(Value::as_str))
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    let url = context
                        .get(if run { "detailsUrl" } else { "targetUrl" })
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    Some(CheckDetail { name, state, url })
                })
                .collect()
        })
        .unwrap_or_default();
    details.into_iter().collect()
}

fn parse_labels(node: &Value) -> Vec<String> {
    node.pointer("/labels/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("name").and_then(|s| s.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
        author: node
            .pointer("/author/login")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        body: text_field(node, "body"),
        head_ref: text_field(node, "headRefName"),
        base_ref: text_field(node, "baseRefName"),
        default_branch: repo
            .pointer("/defaultBranchRef/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        merge_state_status: text_field(node, "mergeStateStatus"),
        check_details: parse_check_details(node),
        head_sha: text_field(node, "headRefOid"),
        base_sha: node
            .pointer("/baseRef/target/oid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        is_draft: node
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        review: parse_review(node),
        checks: parse_checks(node),
        failed_checks: parse_failed_checks(node),
        conflicting: mergeable == Some("CONFLICTING"),
        mergeable_unknown: matches!(mergeable, None | Some("UNKNOWN")),
        behind: node.get("mergeStateStatus").and_then(|v| v.as_str()) == Some("BEHIND"),
        files: Vec::new(),
        files_complete: true,
        labels: parse_labels(node),
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
