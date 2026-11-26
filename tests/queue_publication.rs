#[path = "queue_publication/api.rs"]
mod api;
#[path = "queue_publication/auto_resume.rs"]
mod auto_resume;
mod common;
#[path = "queue_publication/edge_resume.rs"]
mod edge_resume;
#[path = "queue_publication/fixture.rs"]
mod fixture;

use common::git::git_in;
use fixture::{Fixture, REVIEW_URL};
use serde_json::Value;

fn count(lines: &[String], expected: &str) -> usize {
    lines
        .iter()
        .filter(|line| line.as_str() == expected)
        .count()
}

fn assert_waits(summary: &str) {
    for text in [
        "acme/web#2",
        "acme/web#3",
        "acme.core",
        "1.1.0",
        REVIEW_URL,
        "--bump",
        "--state",
        "queue.json",
    ] {
        assert!(
            summary.to_lowercase().contains(text),
            "missing {text} in {summary}"
        );
    }
}

fn assert_waiting_report(report: &Value) {
    let steps = report["steps"].as_array().unwrap();
    for target in ["acme/web#2", "acme/web#3"] {
        let step = steps
            .iter()
            .find(|s| {
                s["kind"] == "bump"
                    && s["target"] == target
                    && s["bump"]["source"] == "queue_publication"
            })
            .unwrap_or_else(|| panic!("{report}"));
        assert_eq!(step["bump"]["pull_request_url"], REVIEW_URL);
        assert_eq!(step["bump"]["version"], "1.1.0");
        assert_eq!(step["bump"]["base"], "main");
        assert_eq!(step["bump"]["ready"], false);
    }
}

fn draft(fixture: &Fixture) -> Value {
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(
        gh.merged.iter().copied().collect::<Vec<_>>(),
        [1, 4],
        "{:?}",
        gh.writes
    );
    assert_eq!(count(&gh.writes, "POST /repos/acme/web/git/refs"), 0);
    assert_eq!(count(&gh.writes, "POST /repos/acme/web/pulls"), 1);
    assert_eq!(gh.writes.len(), 3, "{:?}", gh.writes);
    let seen = fixture.server.seen.lock().unwrap();
    let mut requests = seen.iter().filter_map(|request| request.lines().next());
    for expected in [
        "PUT /repos/acme/sdk/pulls/1/merge ",
        "GET /repos/acme/sdk/actions/jobs/80/logs ",
        "GET /flat/acme.core/index.json ",
        "POST /repos/acme/web/pulls ",
    ] {
        assert!(
            requests.any(|request| request.starts_with(expected)),
            "missing or out of order: {expected}"
        );
    }
    let branch = gh.reviews[0]["head"]["ref"].as_str().unwrap();
    assert_eq!(
        git_in(&gh.repo, &["reflog", "show", "--format=%H", branch])
            .lines()
            .count(),
        1
    );
    gh.reviews[0].clone()
}

#[test]
fn queued_publication_creates_one_review_and_resumes_both_consumers_after_incorporation() {
    let fixture = Fixture::new(true, false);
    let preview = fixture.merge(&["--json"]);
    assert!(
        preview
            .to_lowercase()
            .contains("the version the queue publishes"),
        "{preview}"
    );
    assert!(fixture.gh.lock().unwrap().writes.is_empty());
    assert!(!fixture.dir.join("queue.json").exists());

    let first = fixture.merge(&["--apply", "--yes"]);
    assert_waits(&first);
    let pull = draft(&fixture);
    let previous_writes = fixture.gh.lock().unwrap().writes.clone();
    {
        let mut gh = fixture.gh.lock().unwrap();
        let branch = pull["head"]["ref"].as_str().unwrap();
        assert_eq!(
            git_in(&gh.repo, &["diff", "--name-only", "main", branch]),
            "Web.csproj"
        );
        assert!(
            git_in(&gh.repo, &["show", &format!("{branch}:Web.csproj")])
                .contains("Version=\"1.1.0\"")
        );
        let review = &mut gh.reviews[0];
        review["draft"] = false.into();
        review["reviewDecision"] = "APPROVED".into();
    }
    assert_waits(&fixture.merge(&["--apply", "--yes", "--verbose"]));
    assert_eq!(fixture.gh.lock().unwrap().writes, previous_writes);
    let selected = fixture.merge(&["acme/web#99", "--apply", "--yes", "--json"]);
    let selected: Value = serde_json::from_str(&selected).unwrap();
    assert!(
        selected["preflight"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "acme/web#99")
    );
    assert_eq!(fixture.gh.lock().unwrap().writes, previous_writes);

    fixture.gh.lock().unwrap().integrate();
    fixture.report();
    {
        let mut gh = fixture.gh.lock().unwrap();
        for n in [2, 3] {
            assert_eq!(
                count(
                    &gh.writes,
                    &format!("PUT /repos/acme/web/pulls/{n}/update-branch")
                ),
                1,
                "{:?}",
                gh.writes
            );
        }
        assert_eq!(gh.writes.len(), 5, "{:?}", gh.writes);
        gh.checks = "PENDING";
    }
    fixture.report();
    assert_eq!(fixture.gh.lock().unwrap().writes.len(), 5);
    fixture.gh.lock().unwrap().checks = "SUCCESS";
    fixture.report();
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.merged.iter().copied().collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert_eq!(gh.writes.len(), 7, "{:?}", gh.writes);
    for n in [2, 3] {
        assert_eq!(
            count(&gh.writes, &format!("PUT /repos/acme/web/pulls/{n}/merge")),
            1
        );
    }
}

#[test]
fn missing_new_push_and_version_never_create_a_review_or_merge_a_consumer() {
    let fixture = Fixture::new(false, false);
    for _ in 0..2 {
        let report = fixture.report().to_string().to_lowercase();
        assert!(
            report.contains("acme.core")
                && report.contains("acme/web#2")
                && report.contains("acme/web#3"),
            "{report}"
        );
        let gh = fixture.gh.lock().unwrap();
        assert_eq!(gh.merged.iter().copied().collect::<Vec<_>>(), [1, 4]);
        assert_eq!(gh.writes.len(), 2, "{:?}", gh.writes);
        assert!(gh.reviews.is_empty());
        let refs = git_in(&gh.repo, &["for-each-ref", "refs/heads/pipetrain/"]);
        assert!(refs.is_empty());
    }
}

#[test]
#[ignore = "requires .NET SDK 10 and the net8.0 targeting pack"]
fn queue_publication_locked_restore_resolves_the_proven_nuget_version() {
    let fixture = Fixture::new(true, true);
    assert_waiting_report(&fixture.report());
    let pull = draft(&fixture);
    let gh = fixture.gh.lock().unwrap();
    let branch = pull["head"]["ref"].as_str().unwrap();
    assert_eq!(
        git_in(&gh.repo, &["diff", "--name-only", "main", branch])
            .lines()
            .collect::<Vec<_>>(),
        ["Web.csproj", "packages.lock.json"]
    );
    let lock: Value = serde_json::from_str(&git_in(
        &gh.repo,
        &["show", &format!("{branch}:packages.lock.json")],
    ))
    .unwrap();
    assert_eq!(
        lock["dependencies"]["net8.0"]["Acme.Core"]["resolved"],
        "1.1.0"
    );
}

#[test]
fn lost_creation_reply_reconciles_the_matching_draft_and_closed_draft_stays_held() {
    let fixture = Fixture::new(true, false);
    fixture.gh.lock().unwrap().lose_create_reply = true;
    fixture.report();
    assert_waiting_report(&fixture.report());
    draft(&fixture);
    {
        let mut gh = fixture.gh.lock().unwrap();
        gh.reviews[0]["state"] = "closed".into();
    }
    let report = fixture.report().to_string();
    assert!(report.contains("closed"), "{report}");
    draft(&fixture);
}

#[test]
fn successful_push_log_without_the_exact_version_in_the_feed_does_not_prepare() {
    let fixture = Fixture::new(true, false);
    fixture.gh.lock().unwrap().feed_has_version = false;
    let report = fixture.report().to_string().to_lowercase();
    assert!(
        report.contains("acme.core") && report.contains("feed"),
        "{report}"
    );
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.merged.iter().copied().collect::<Vec<_>>(), [1, 4]);
    assert_eq!(gh.writes.len(), 2, "{:?}", gh.writes);
}

#[test]
fn two_consumer_bases_need_distinct_branches_and_reviews() {
    let fixture = Fixture::new(true, false);
    {
        let mut gh = fixture.gh.lock().unwrap();
        git_in(&gh.repo, &["branch", "release", "main"]);
        gh.second_base = "release";
    }
    let report = fixture.report();
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.reviews.len(), 2, "{report}");
    let bases: std::collections::BTreeSet<_> = gh
        .reviews
        .iter()
        .map(|p| p["base"]["ref"].as_str().unwrap())
        .collect();
    assert_eq!(bases, std::collections::BTreeSet::from(["release", "main"]));
    assert_ne!(gh.reviews[0]["head"]["ref"], gh.reviews[1]["head"]["ref"]);
    assert_eq!(count(&gh.writes, "POST /repos/acme/web/pulls"), 2);
}

#[test]
fn two_candidate_publisher_prs_hold_both_consumers_without_a_draft() {
    let fixture = Fixture::new(true, false);
    let report = fixture.merge(&["acme/sdk#5", "--apply", "--yes", "--json"]);
    let report: Value = serde_json::from_str(&report).unwrap();
    for target in ["acme/web#2", "acme/web#3"] {
        let step = report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["target"] == target && s["kind"] == "bump")
            .unwrap();
        assert_eq!(
            step["bump"]["publishers"],
            serde_json::json!(["acme/sdk#1", "acme/sdk#5"])
        );
        assert_eq!(step["bump"]["version"], Value::Null);
        assert_eq!(step["bump"]["ready"], false);
    }
    let gh = fixture.gh.lock().unwrap();
    assert!(gh.reviews.is_empty());
    assert!(gh.writes.iter().all(|w| !w.contains("/web/")));
}

#[test]
fn failed_publisher_deploy_leaves_consumers_held_while_independent_work_advances() {
    let fixture = Fixture::new(true, false);
    fixture.gh.lock().unwrap().deploy_conclusion = "failure";
    let text = fixture.merge_expecting(&["--apply", "--yes", "--json"], &[1]);
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["summary"]["deploy_failed"], 1);
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.merged.iter().copied().collect::<Vec<_>>(), [1, 4]);
    assert!(gh.reviews.is_empty());
    assert!(gh.writes.iter().all(|w| !w.contains("/web/")));
}

#[test]
fn branch_check_lost_after_push_is_reconciled_with_one_remote_commit() {
    let fixture = Fixture::new(true, false);
    fixture.gh.lock().unwrap().hide_pushed_ref = true;
    fixture.report();
    assert!(!fixture.gh.lock().unwrap().hide_pushed_ref);
    assert_waiting_report(&fixture.report());
    draft(&fixture);
}

#[test]
fn a_push_to_another_feed_with_the_same_version_is_not_publication_proof() {
    let fixture = Fixture::new(true, false);
    fixture.gh.lock().unwrap().foreign_feed = true;
    let report = fixture.report();
    let gh = fixture.gh.lock().unwrap();
    assert!(gh.reviews.is_empty(), "{report}");
    assert!(gh.writes.iter().all(|w| !w.contains("/web/")));
}

#[test]
fn conflicted_consumer_waits_until_manual_incorporation_and_new_green_checks() {
    let fixture = Fixture::new(true, false);
    fixture.report();
    {
        let mut gh = fixture.gh.lock().unwrap();
        gh.integrate();
        gh.conflicting_update = Some(2);
    }
    let report = fixture.report();
    let conflict = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "bump" && s["target"] == "acme/web#2")
        .unwrap();
    assert_eq!(conflict["bump"]["ready"], false);
    assert!(
        conflict["detail"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("conflict"),
        "{conflict}"
    );
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert!(!gh.merged.contains(&2));
        gh.conflicting_update = None;
        gh.incorporate(2);
        gh.checks = "PENDING";
    }
    fixture.report();
    assert!(!fixture.gh.lock().unwrap().merged.contains(&2));
    fixture.gh.lock().unwrap().checks = "SUCCESS";
    fixture.report();
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.merged.iter().copied().collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert_eq!(gh.reviews.len(), 1);
}

#[test]
fn retargeted_consumer_cannot_reuse_a_draft_against_its_previous_base() {
    let fixture = Fixture::new(true, false);
    fixture.report();
    let writes = {
        let mut gh = fixture.gh.lock().unwrap();
        git_in(&gh.repo, &["branch", "release", "main"]);
        gh.second_base = "release";
        gh.writes.clone()
    };
    let report = fixture.report();
    let held = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "bump" && s["target"] == "acme/web#3")
        .unwrap();
    assert_eq!(held["bump"]["ready"], false);
    assert!(
        held["detail"].as_str().unwrap().contains("base changed"),
        "{held}"
    );
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.writes, writes);
    assert_eq!(gh.reviews.len(), 1);
    assert_eq!(gh.reviews[0]["base"]["ref"], "main");
}

#[test]
fn each_consumer_is_held_by_the_literal_pin_on_its_own_head() {
    let fixture = Fixture::new(true, false);
    let checkout = fixture.dir.join("web");
    let remote = fixture.gh.lock().unwrap().repo.clone();
    git_in(&checkout, &["switch", "main"]);
    let project = checkout.join("Web.csproj");
    let original = std::fs::read_to_string(&project).unwrap();
    std::fs::write(&project, original.replace("1.0.0", "1.1.0")).unwrap();
    git_in(
        &checkout,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-am",
            "Advance base pin",
        ],
    );
    git_in(&checkout, &["switch", "feature/2"]);
    git_in(
        &checkout,
        &["-c", "commit.gpgsign=false", "merge", "--no-edit", "main"],
    );
    git_in(&checkout, &["switch", "main"]);
    std::fs::write(&project, original).unwrap();
    git_in(
        &checkout,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-am",
            "Restore the previous base pin",
        ],
    );
    git_in(
        &checkout,
        &["push", remote.to_str().unwrap(), "main", "feature/2"],
    );
    assert_eq!(
        git_in(&remote, &["diff", "--name-only", "main...feature/2"]),
        "Feature2.cs"
    );
    assert_eq!(
        git_in(&remote, &["diff", "--name-only", "main...feature/3"]),
        "Feature3.cs"
    );

    let report = fixture.report();
    let queued: Vec<_> = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["bump"]["source"] == "queue_publication")
        .collect();
    assert_eq!(queued.len(), 1, "{report}");
    assert_eq!(queued[0]["target"], "acme/web#3");
    assert_eq!(queued[0]["bump"]["version"], "1.1.0");
    assert_eq!(queued[0]["bump"]["pull_request_url"], REVIEW_URL);
    let gh = fixture.gh.lock().unwrap();
    assert!(!gh.merged.contains(&3));
    assert_eq!(gh.reviews.len(), 1);
    assert!(git_in(&remote, &["show", "feature/2:Web.csproj"]).contains("Version=\"1.1.0\""));
    assert!(git_in(&remote, &["show", "feature/3:Web.csproj"]).contains("Version=\"1.0.0\""));
}
