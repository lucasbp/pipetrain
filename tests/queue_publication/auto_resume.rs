use super::api::Github;
use super::fixture::{Fixture, REVIEW_URL};
use super::{count, draft};
use serde_json::Value;
use std::fs;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(45);

pub(super) struct Running<'a> {
    fixture: &'a Fixture,
    child: Child,
}

impl<'a> Running<'a> {
    pub(super) fn start(fixture: &'a Fixture) -> Self {
        Self::launch(
            fixture,
            fixture.command(&["--apply", "--yes", "--json", "--timeout-minutes", "1"]),
        )
    }

    pub(super) fn launch(fixture: &'a Fixture, mut command: Command) -> Self {
        let child = command
            .stdout(fs::File::create(fixture.dir.join("auto.stdout")).unwrap())
            .stderr(fs::File::create(fixture.dir.join("auto.stderr")).unwrap())
            .spawn()
            .unwrap();
        Self { fixture, child }
    }

    fn diagnostic(&self) -> String {
        format!(
            "{}\n{}",
            self.output("auto.stderr"),
            self.output("auto.stdout")
        )
    }

    fn output(&self, name: &str) -> String {
        fs::read_to_string(self.fixture.dir.join(name)).unwrap_or_default()
    }

    pub(super) fn until(&mut self, what: &str, condition: impl Fn(&Github) -> bool) {
        let started = Instant::now();
        loop {
            if condition(&self.fixture.gh.lock().unwrap()) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("merge exited {status} before {what}: {}", self.diagnostic());
            }
            assert!(
                started.elapsed() < LIMIT,
                "timed out waiting for {what}. Writes: {:?}. {}",
                self.fixture.gh.lock().unwrap().writes,
                self.diagnostic()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub(super) fn finish(&mut self, expect: i32, limit: Duration) -> Value {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(expect), "{}", self.diagnostic());
                let stderr = self.output("auto.stderr").to_ascii_lowercase();
                assert!(
                    !stderr.contains("type yes") && !stderr.contains("without a terminal"),
                    "{stderr}"
                );
                return serde_json::from_str(&self.output("auto.stdout")).unwrap();
            }
            assert!(
                started.elapsed() < limit,
                "merge did not finish: {}",
                self.diagnostic()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn waiting_for_review(&mut self) {
        self.until("an independent merge and open draft", |gh| {
            gh.reviews.len() == 1 && gh.merged.contains(&4)
        });
        let path = self.fixture.dir.join("queue.json");
        self.until("both durable review links", |_| {
            fs::read(&path)
                .ok()
                .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
                .is_some_and(|state| {
                    state["bump_behind"].as_array().is_some_and(|entries| {
                        entries.iter().filter(|e| e["number"] == 99).count() == 2
                    })
                })
        });
        let start = self.fixture.gh.lock().unwrap().review_reads.len();
        self.until("review polling after initial reconciliation", |gh| {
            gh.review_reads[start..]
                .windows(2)
                .any(|pair| pair[1] - pair[0] >= Duration::from_millis(500))
        });
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn assert_merged(fixture: &Fixture, report: &Value) {
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(gh.merged.iter().copied().collect::<Vec<_>>(), [1, 2, 3, 4]);
    for target in ["sdk/pulls/1", "web/pulls/2", "web/pulls/3", "other/pulls/4"] {
        assert_eq!(
            count(&gh.writes, &format!("PUT /repos/acme/{target}/merge")),
            1
        );
    }
    for number in [2, 3] {
        assert_eq!(
            count(
                &gh.writes,
                &format!("PUT /repos/acme/web/pulls/{number}/update-branch")
            ),
            1
        );
    }
    assert_eq!(count(&gh.writes, "POST /repos/acme/web/pulls"), 1);
    assert_eq!(gh.writes.len(), 7, "{:?}", gh.writes);
    assert_eq!(gh.reviews.len(), 1);
    assert_eq!(report["summary"]["failed"], 0);
    assert_eq!(report["summary"]["stopped"], 0);
}

#[test]
fn one_merge_process_waits_for_shared_bump_incorporation_review_and_checks() {
    let fixture = Fixture::new(true, false);
    let mut run = Running::start(&fixture);
    run.waiting_for_review();
    let (start, request_mark) = {
        let mut gh = fixture.gh.lock().unwrap();
        gh.reviews[0]["draft"] = false.into();
        gh.reviews[0]["reviewDecision"] = "APPROVED".into();
        (
            gh.review_reads.len(),
            fixture.server.seen.lock().unwrap().len(),
        )
    };
    run.until(
        "two more polls while the approved bump remains open",
        |gh| gh.review_reads.len() >= start + 2,
    );
    draft(&fixture);
    {
        let gh = fixture.gh.lock().unwrap();
        for pair in gh.review_reads[start - 1..start + 2].windows(2) {
            assert!(
                pair[1] - pair[0] >= Duration::from_millis(500),
                "shared bump polled twice per round: {:?}",
                gh.review_reads
            );
        }
        let calls = fixture.server.seen.lock().unwrap();
        assert!(
            !calls[request_mark..]
                .iter()
                .any(|call| call.starts_with("GET /repos/acme/sdk/actions/")
                    || call.starts_with("GET /flat/")
                    || call.contains("/git/trees/")),
            "unchanged open draft repeated publication or planning reads"
        );
    }
    let old_heads = {
        let mut gh = fixture.gh.lock().unwrap();
        gh.hold_updated_heads = true;
        gh.integrate();
        [gh.head(2), gh.head(3)]
    };
    for (n, previous) in [2, 3].into_iter().zip(old_heads) {
        run.until("pending checks on the incorporated head", |gh| {
            gh.seen_readiness.iter().any(|(number, _, checks, review)| {
                *number == n && *checks == "PENDING" && *review == "REVIEW_REQUIRED"
            })
        });
        let updated = {
            let mut gh = fixture.gh.lock().unwrap();
            let sha = gh.head(n);
            assert_ne!(sha, previous);
            assert!(!gh.merged.contains(&n));
            gh.readiness.insert(sha.clone(), ("PENDING", "APPROVED"));
            sha
        };
        run.until("approved head still waiting for CI", |gh| {
            gh.seen_readiness
                .contains(&(n, updated.clone(), "PENDING", "APPROVED"))
        });
        {
            let mut gh = fixture.gh.lock().unwrap();
            assert!(!gh.merged.contains(&n));
            gh.readiness
                .insert(updated.clone(), ("SUCCESS", "APPROVED"));
        }
    }
    let report = run.finish(0, LIMIT);
    assert_merged(&fixture, &report);
    assert_eq!(report["summary"]["merged"], 4);
    assert_eq!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|step| step["kind"] == "merge" && step["status"] == "done")
            .count(),
        4
    );
}

#[test]
fn failing_ci_on_one_updated_head_does_not_merge_it_or_hold_its_healthy_peer() {
    let fixture = Fixture::new(true, false);
    let mut run = Running::start(&fixture);
    run.waiting_for_review();
    {
        let mut gh = fixture.gh.lock().unwrap();
        gh.hold_updated_heads = true;
        gh.integrate();
    }
    run.until("checks on both updated heads", |gh| {
        [2, 3].iter().all(|n| {
            gh.seen_readiness
                .iter()
                .any(|(number, _, checks, _)| number == n && *checks == "PENDING")
        })
    });
    {
        let mut gh = fixture.gh.lock().unwrap();
        for (n, verdict) in [(2, "FAILURE"), (3, "SUCCESS")] {
            let sha = gh.head(n);
            gh.readiness.insert(sha, (verdict, "APPROVED"));
        }
    }
    let report = run.finish(5, LIMIT);
    let gh = fixture.gh.lock().unwrap();
    assert!(!gh.merged.contains(&2));
    assert!(gh.merged.contains(&3));
    assert!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["target"] == "acme/web#2"
                && step["status"] == "stopped"
                && step["detail"].as_str().unwrap().contains("checks failing")),
        "{report}"
    );
}

#[test]
fn closed_unmerged_shared_draft_stops_without_waiting_for_timeout() {
    let fixture = Fixture::new(true, false);
    let mut run = Running::start(&fixture);
    run.waiting_for_review();
    fixture.gh.lock().unwrap().reviews[0]["state"] = "closed".into();
    let report = run.finish(5, LIMIT);
    let text = report.to_string().to_lowercase();
    assert!(
        text.contains(REVIEW_URL) && text.contains("closed"),
        "{report}"
    );
    draft(&fixture);
}

#[test]
fn interrupted_review_wait_resumes_the_same_draft_and_queue_without_repeating_writes() {
    let fixture = Fixture::new(true, false);
    let mut first = Running::start(&fixture);
    first.waiting_for_review();
    draft(&fixture);
    first.child.kill().unwrap();
    assert!(!first.child.wait().unwrap().success());
    drop(first);

    let mut resumed = Running::start(&fixture);
    let calls = fixture.gh.lock().unwrap().review_reads.len();
    resumed.until("the saved review was checked again", |gh| {
        gh.review_reads.len() > calls
    });
    draft(&fixture);
    fixture.gh.lock().unwrap().integrate();
    let report = resumed.finish(0, LIMIT);
    assert_merged(&fixture, &report);
    assert_eq!(report["summary"]["merged"], 2);
}

#[test]
#[ignore = "exercises the full one-minute review deadline"]
fn an_open_draft_times_out_without_replanning_the_unchanged_queue() {
    let fixture = Fixture::new(true, false);
    let mut run = Running::start(&fixture);
    run.waiting_for_review();
    let report = run.finish(5, Duration::from_secs(85));
    draft(&fixture);
    let text = report.to_string().to_lowercase();
    assert!(
        text.contains(REVIEW_URL)
            && (text.contains("timeout")
                || text.contains("timed out")
                || text.contains("deadline")),
        "{report}"
    );
    assert!(
        fixture.gh.lock().unwrap().review_reads.len() < 80,
        "poll count multiplied by the two held consumers"
    );
}

#[test]
fn a_green_rollup_from_the_previous_head_does_not_release_the_consumer() {
    let fixture = Fixture::new(true, false);
    fixture.report();
    let previous = {
        let mut gh = fixture.gh.lock().unwrap();
        let previous = gh.head(2);
        gh.integrate();
        previous
    };
    fixture.report();
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert_ne!(gh.head(2), previous);
        gh.reported_checks_sha.insert(2, previous);
    }
    let report = fixture.report();
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert!(!gh.merged.contains(&2), "{report}");
        assert!(
            report["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["kind"] == "bump"
                    && s["target"] == "acme/web#2"
                    && s["bump"]["ready"] == false),
            "{report}"
        );
        gh.reported_checks_sha.remove(&2);
    }
    let report = fixture.report();
    assert_merged(&fixture, &report);
}

#[test]
fn an_unrelated_commit_on_top_of_the_requested_update_stays_held() {
    let fixture = Fixture::new(true, false);
    fixture.report();
    fixture.gh.lock().unwrap().integrate();
    fixture.report();
    {
        let gh = fixture.gh.lock().unwrap();
        let updated = gh.head(2);
        let tree =
            crate::common::git::git_in(&gh.repo, &["rev-parse", &format!("{updated}^{{tree}}")]);
        let other = crate::common::git::git_in(
            &gh.repo,
            &["commit-tree", &tree, "-p", &updated, "-m", "Another change"],
        );
        crate::common::git::git_in(
            &gh.repo,
            &["update-ref", "refs/heads/feature/2", &other, &updated],
        );
    }
    let report = fixture.report();
    let gh = fixture.gh.lock().unwrap();
    assert!(!gh.merged.contains(&2), "{report}");
    assert_eq!(
        count(&gh.writes, "PUT /repos/acme/web/pulls/2/update-branch"),
        1
    );
    assert_eq!(gh.reviews.len(), 1);
}

fn dependency_queue(with_failed_deploy: bool) {
    let fixture = Fixture::new(true, false);
    {
        let mut gh = fixture.gh.lock().unwrap();
        gh.open.extend([
            (1, "sdk"),
            (2, "web"),
            (3, "web"),
            (4, "other"),
            (5, "portal"),
            (6, "reports"),
        ]);
        gh.dependencies.extend([
            (5, "depends-on: acme/web#2"),
            (6, "depends-on: acme/portal#5"),
        ]);
        if with_failed_deploy {
            gh.open.extend([(7, "broken"), (8, "export")]);
            gh.dependencies
                .insert(8, "depends-on: acme/web#2, acme/broken#7");
        }
    }
    let command = fixture.selected_command(
        &["--all"],
        &["--apply", "--yes", "--json", "--timeout-minutes", "1"],
    );
    let mut run = Running::launch(&fixture, command);
    run.waiting_for_review();
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert!(gh.merged.contains(&1) && gh.merged.contains(&4));
        assert!(![2, 3, 5, 6, 8].iter().any(|n| gh.merged.contains(n)));
        gh.open.insert(9, "late");
        gh.integrate();
    }
    let report = run.finish(if with_failed_deploy { 1 } else { 0 }, LIMIT);
    let gh = fixture.gh.lock().unwrap();
    let targets = &gh.merged;
    let expected: Vec<_> = (1..=if with_failed_deploy { 7 } else { 6 }).collect();
    assert_eq!(
        targets.iter().copied().collect::<Vec<_>>(),
        expected,
        "{report}"
    );
    let writes: Vec<_> = gh
        .writes
        .iter()
        .filter(|line| line.ends_with("/merge"))
        .collect();
    assert_eq!(writes.len(), expected.len(), "{writes:?}");
    let order = |pr: &str| writes.iter().position(|line| line.contains(pr)).unwrap();
    assert!(order("/web/pulls/2/") < order("/portal/pulls/5/"));
    assert!(order("/portal/pulls/5/") < order("/reports/pulls/6/"));
    assert!(
        !gh.writes
            .iter()
            .any(|line| line.contains("/late/") || line.contains("/export/"))
    );
    assert_eq!(gh.reviews.len(), 1);
    if with_failed_deploy {
        assert_eq!(report["summary"]["deploy_failed"], 1, "{report}");
        assert!(
            report["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step["target"] == "acme/broken#7" && step["status"] == "failed"),
            "{report}"
        );
    }
}

#[test]
fn resumed_all_merges_selected_dependents_in_order_and_ignores_newly_opened_prs() {
    dependency_queue(false);
}

#[test]
fn resumed_dependent_keeps_the_failed_deployment_of_its_other_prerequisite() {
    dependency_queue(true);
}
