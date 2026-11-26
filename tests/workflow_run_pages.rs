mod common;

use common::serve_handler;
use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client, Run};
use serde_json::{Value, json};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
type Listing = Result<(Vec<Run>, bool), ApiError>;

fn page(first_id: u64, count: u64, total: Option<u64>) -> Value {
    let runs: Vec<_> = (first_id..first_id + count)
        .map(|id| {
            json!({
                "id": id, "workflow_id": 5, "name": "CI", "path": ".github/workflows/ci.yml",
                "status": "completed", "conclusion": "success", "head_sha": SHA,
                "html_url": format!("https://example.test/runs/{id}")
            })
        })
        .collect();
    let mut body = json!({"workflow_runs": runs});
    if let Some(total) = total {
        body["total_count"] = json!(total);
    }
    body
}

fn read(pages: Vec<(u16, Value)>, push_only: bool, budget: u32) -> (Listing, Vec<String>) {
    let replies: std::collections::HashMap<_, _> = expected_paths(pages.len(), push_only)
        .into_iter()
        .zip(pages)
        .collect();
    let server =
        serve_handler(move |line| replies.get(line).cloned().unwrap_or((404, Value::Null)));
    let config = ApiConfig {
        max_requests: budget,
        ..ApiConfig::default()
    };
    let mut client = Client::with_base_url("fake".into(), &config, false, &server.base);
    let result = client.workflow_runs("acme/api", SHA, push_only);
    let paths = server
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            request
                .lines()
                .next()
                .unwrap()
                .trim_end_matches(" HTTP/1.1")
                .to_owned()
        })
        .collect();
    (result, paths)
}

fn expected_paths(count: usize, push_only: bool) -> Vec<String> {
    let event = if push_only { "&event=push" } else { "" };
    let first = format!("GET /repos/acme/api/actions/runs?head_sha={SHA}{event}&per_page=100");
    (1..=count)
        .map(|page| {
            if page == 1 {
                first.clone()
            } else {
                format!("{first}&page={page}")
            }
        })
        .collect()
}

fn successful_pages(size: u64, total: Option<u64>) -> Vec<(u16, Value)> {
    (0..size)
        .step_by(100)
        .map(|start| (200, page(start + 1, (size - start).min(100), total)))
        .collect()
}

#[test]
fn exact_total_reads_each_page_once_including_the_last_workflow() {
    for (size, count) in [(101, 2), (201, 3), (1000, 10), (100, 1), (1, 1)] {
        for push_only in [false, true] {
            let (result, paths) = read(successful_pages(size, Some(size)), push_only, 12);
            let (runs, complete) = result.unwrap();
            assert_eq!(
                (paths.len(), runs.len(), complete),
                (count, size as usize, true),
                "{size} workflows"
            );
            assert_eq!(paths, expected_paths(count, push_only));
            assert_eq!(
                runs.iter().map(|r| r.id).collect::<Vec<_>>(),
                (1..=size).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn a_missing_total_needs_a_short_final_page_within_the_search_limit() {
    for (size, count, complete) in [
        (0, 1, true),
        (99, 1, true),
        (100, 2, true),
        (201, 3, true),
        (1000, 10, false),
    ] {
        let mut pages = successful_pages(size, None);
        if size.is_multiple_of(100) && size < 1000 {
            pages.push((200, page(size + 1, 0, None)));
        }
        let (result, paths) = read(pages, false, 12);
        let (runs, actual_complete) = result.unwrap();
        assert_eq!((runs.len(), actual_complete), (size as usize, complete));
        assert_eq!(paths, expected_paths(count, false));
    }
}

#[test]
fn a_total_beyond_the_search_limit_cannot_confirm_and_never_requests_page_eleven() {
    let (result, paths) = read(successful_pages(1000, Some(1001)), false, 12);
    let (_, complete) = result.unwrap();
    assert!(!complete);
    assert!((1..=10).contains(&paths.len()));
}

#[test]
fn a_changed_total_overlap_or_unreadable_tail_cannot_confirm_the_first_page() {
    for tail in [
        page(101, 2, Some(102)),
        page(100, 1, Some(101)),
        page(101, 0, Some(101)),
        json!({"total_count": 101, "workflow_runs": null}),
        json!({"total_count": 101, "workflow_runs": [{}]}),
    ] {
        let mut first = page(1, 100, Some(101));
        first["workflow_runs"][0]["conclusion"] = json!("failure");
        let (result, paths) = read(vec![(200, first), (200, tail)], false, 3);
        let (runs, complete) = result.unwrap();
        assert!(!complete);
        assert!(
            runs.iter()
                .any(|run| run.id == 1 && run.conclusion.as_deref() == Some("failure"))
        );
        assert_eq!(paths, expected_paths(2, false));
    }
}

#[test]
fn a_short_page_disagreeing_with_its_total_is_incomplete() {
    let (result, paths) = read(vec![(200, page(1, 3, Some(101)))], false, 2);
    let (runs, complete) = result.unwrap();
    assert_eq!((runs.len(), complete), (3, false));
    assert_eq!(paths, expected_paths(1, false));
}

#[test]
fn a_single_page_requires_positive_run_ids_and_matching_shas() {
    for (field, invalid) in [
        ("id", Value::Null),
        ("id", json!(0)),
        ("head_sha", json!("other")),
    ] {
        let mut body = page(1, 1, Some(1));
        body["workflow_runs"][0][field] = invalid;
        let (result, paths) = read(vec![(200, body)], false, 2);
        assert!(!result.unwrap().1);
        assert_eq!(paths, expected_paths(1, false));
    }
}

#[test]
fn later_page_errors_and_request_budget_stops_reach_the_caller() {
    let pages = vec![(200, page(1, 100, Some(101))), (401, Value::Null)];
    let (result, paths) = read(pages.clone(), false, 3);
    assert_eq!(result.unwrap_err(), ApiError::Unauthorized);
    assert_eq!(paths, expected_paths(2, false));
    let (result, paths) = read(pages, false, 1);
    assert!(matches!(result, Err(ApiError::Stopped { .. })));
    assert_eq!(paths, expected_paths(1, false));
}
