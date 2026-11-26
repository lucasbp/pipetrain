mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Server, TempDir, pr, serve_requests};
use pipetrain::command::merge::behind::Options;
use pipetrain::command::merge::{Hooks, Outcome, Settings, merge_plan};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::discovery::{Scope, Skipped};
use pipetrain::github::Client;
use pipetrain::merge::{BumpBehind, DependencyFailure, Kind, Status, Store};
use pipetrain::model::Pr;
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::planner::behind::Need;
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const CAUSE: &str = "needs a bump of Acme.Core to 1.4.0";

#[test]
fn missing_commit_comparison_does_not_write_an_update_or_release_the_pr() {
    rejected_publication(false, "ancestry was not reported");
}

#[test]
fn existing_multi_commit_pr_is_not_adopted_or_updated() {
    rejected_publication(true, "does not contain exactly one commit");
}

fn rejected_publication(existing: bool, expected: &str) {
    use pipetrain::command::merge::behind::{GithubGateway, published};
    use pipetrain::planner::behind::QueuedBump;

    let pr = pr("acme/web", 7, "topic", "main");
    let head = pr.head_sha.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let branch = "pipetrain/bump/acme.core-1.4.0";
    let server = serve_requests(move |line, payload| {
        observed.lock().unwrap().push(line.to_owned());
        match line {
            "GET /repos/acme/web/pulls/7" => (
                200,
                json!({"state":"open","base":{"ref":"main"},"head":{"repo":{"full_name":"acme/web"},"ref":"topic","sha":head}}),
            ),
            "GET /repos/acme/web/git/ref/heads/main" => (200, json!({"object":{"sha":sha(1)}})),
            "GET /repos/acme/web/pulls/14" => (
                200,
                json!({
                    "html_url":"https://github.com/acme/web/pull/14", "commits":2,
                    "base":{"ref":"main","sha":sha(1)}, "head":{"repo":{"full_name":"acme/web"},"ref":branch,"sha":sha(14)}
                }),
            ),
            "POST /graphql" => {
                let version = if payload.contains(&head) {
                    "1.0.0"
                } else {
                    "1.4.0"
                };
                let text = format!(
                    r#"<Project><PackageReference Include="Acme.Core" Version="{version}"/></Project>"#
                );
                (200, json!({"data":{"repository":{"f0":{"text":text}}}}))
            }
            _ if existing && line.ends_with(branch) => (200, json!({"object":{"sha":sha(14)}})),
            _ if existing && line.contains(&format!("{branch}&base=")) => {
                (200, json!([{"number":14}]))
            }
            _ if line.contains("/pulls?") => (200, json!([])),
            _ if line.contains("/git/trees/") => (
                200,
                json!({"truncated":false,"tree":[{"type":"blob","path":"App.csproj"}]}),
            ),
            _ if line.contains("/compare/") => (200, json!({})),
            _ => (404, json!({})),
        }
    });
    let cfg = Config::parse("orgs=['acme']").unwrap();
    let mut client =
        Client::with_base_url("ghp_FAKE0123456789".into(), &cfg.api, false, &server.base);
    let contract = QueuedBump {
        id: pr.key.to_string(),
        base: pr.base_ref.clone(),
        head: pr.head_ref.clone(),
        sha: pr.head_sha.clone(),
        package: "Acme.Core".into(),
        from: "1.0.0".into(),
        owners: vec!["acme/core".into()],
        publishers: vec!["acme/core#3".into()],
    };
    use pipetrain::publication::{
        ExactFeed, PublicationResult, PublisherExecution, PublisherRun, verify,
    };
    struct Feed;
    impl ExactFeed for Feed {
        fn contains(&mut self, package: &str, version: &str) -> anyhow::Result<bool> {
            Ok(package == "Acme.Core" && version == "1.4.0")
        }
    }
    let run = PublisherRun {
        repository: "acme/core".into(),
        head_sha: sha(1),
        id: 1,
        succeeded: true,
        push_step_succeeded: true,
        push_step_log: "Pushing Acme.Core.1.4.0.nupkg to feed\nYour package was pushed.".into(),
    };
    let execution = PublisherExecution {
        repository: run.repository.clone(),
        merge_sha: run.head_sha.clone(),
        run_id: run.id,
        published_this_execution: true,
    };
    let PublicationResult::Published { evidence } =
        verify(&execution, &run, "Acme.Core", &mut Feed).unwrap()
    else {
        panic!("expected publication proof")
    };
    let (step, ready) = published(
        &contract,
        &evidence,
        &pr,
        &mut Store::in_memory(),
        &mut GithubGateway {
            client: &mut client,
            cfg: &cfg,
            options: &Options::default(),
        },
    );
    assert!(!ready);
    assert!(step.detail.contains(expected), "{}", step.detail);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|line| line.starts_with("GET ") || line == "POST /graphql")
    );
}

fn sha(number: u64) -> String {
    format!("{number:040x}")
}

fn merged_sha(number: u64) -> String {
    sha(0x1000 + number)
}

#[derive(Default)]
struct Script {
    bump_pull: &'static str,
    conflict: bool,
    log: Vec<String>,
}

impl Script {
    fn node(&self, number: u64, repo: &str) -> Value {
        json!({
            "number": number, "title": "change", "body": "",
            "url": format!("https://github.com/acme/{repo}/pull/{number}"),
            "headRefName": format!("feat/{number}"), "baseRefName": "main",
            "headRefOid": sha(number), "baseRef": {"target": {"oid": "b".repeat(40)}},
            "state": "OPEN",
            "isDraft": false, "reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN",
            "repository": {"name": repo, "owner": {"login": "acme"}, "defaultBranchRef": {"name": "main"}},
            "latestOpinionatedReviews": {"nodes": [{"state": "APPROVED"}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS", "contexts": {"nodes": []}}}}]}
        })
    }

    fn respond(&mut self, line: &str, payload: &str) -> (u16, Value) {
        self.log.push(line.to_string());
        if line.contains("/repos/acme/web/pulls/101") {
            return match self.bump_pull {
                "open" => (200, json!({"state": "open", "merged": false})),
                "merged" => (200, json!({"state": "closed", "merged": true})),
                "closed" => (200, json!({"state": "closed", "merged": false})),
                _ => (500, json!({"message": "boom"})),
            };
        }
        if line.starts_with("PUT ") && line.ends_with("/update-branch") {
            return if self.conflict {
                (
                    422,
                    json!({"message": "merge conflict between base and head"}),
                )
            } else {
                (202, json!({"message": "Updating pull request branch."}))
            };
        }
        if line.starts_with("POST /repos/") {
            return (500, json!({"message": "no write expected"}));
        }
        if line == "POST /graphql" {
            let body: Value = serde_json::from_str(payload).unwrap();
            if body.pointer("/variables/e0").is_some() {
                return (
                    200,
                    json!({"data": {"repository": {"f0": {"text": "on: push"}}}}),
                );
            }
            if body["query"].as_str().unwrap().contains("search(query:") {
                return (
                    200,
                    json!({"data": {"search": {"nodes": [], "issueCount": 0, "pageInfo": {"hasNextPage": false}}}}),
                );
            }
            if body.pointer("/variables/q0").is_some() {
                return (
                    200,
                    json!({"data": {"t0": {"ref": null}, "t1": {"ref": null}}}),
                );
            }
            let number = body["variables"]["n"].as_u64().unwrap();
            let repo = body["variables"]["r"].as_str().unwrap();
            return (
                200,
                json!({"data": {"repository": {"pullRequest": self.node(number, repo)}}}),
            );
        }
        if line.starts_with("PUT ") {
            let number = line.split('/').nth(5).unwrap().parse().unwrap();
            return (200, json!({"merged": true, "sha": merged_sha(number)}));
        }
        if line.contains("/git/trees/") {
            return (
                200,
                json!({"truncated": false, "tree": [{"type": "blob", "path": ".github/workflows/deploy.yml"}]}),
            );
        }
        if line.contains("/files?") {
            return (
                200,
                json!([{"filename": "src/app.rs", "status": "modified"}]),
            );
        }
        if line.contains("/git/ref/heads/") {
            return (200, json!({"object": {"sha": "d".repeat(40)}}));
        }
        if line.contains("/compare/") {
            return (200, json!({"status": "ahead", "ahead_by": 1}));
        }
        if line.contains("/actions/runs?") {
            return (
                200,
                json!({"total_count": 1, "workflow_runs": [{
                    "id": 1, "name": "deploy", "path": ".github/workflows/deploy.yml",
                    "status": "completed", "conclusion": "success",
                    "html_url": "https://example.test/acme/api/actions/runs/1"
                }]}),
            );
        }
        if let Some(number) = line
            .rsplit('/')
            .next()
            .and_then(|value| value.parse::<u64>().ok())
        {
            return (
                200,
                json!({
                    "state": "open", "merged": false, "head": {"sha": sha(number)},
                    "base": {"ref": "main"}, "mergeable_state": "clean", "changed_files": 1,
                    "merge_commit_sha": merged_sha(number)
                }),
            );
        }
        (404, json!({"message": "unexpected request"}))
    }
}

struct Fixture {
    directory: TempDir,
    script: Arc<Mutex<Script>>,
    server: Server,
    cfg: Config,
}

impl Fixture {
    fn new(bump_pull: &'static str, saved: bool) -> Self {
        let directory = TempDir::new("bump-behind");
        let text = "orgs = ['acme']\n[api]\nmax_requests = 300\nmax_per_minute = 10000\nmin_remaining = 0\n[merge]\nstate_file = 'state.json'\npoll_seconds = 1\ntimeout_minutes = 1\nno_run_grace_seconds = 120\n";
        fs::write(directory.join("config.toml"), text).unwrap();
        fs::write(
            directory.join("state.json"),
            "{\"schema\":2,\"pending\":[]}",
        )
        .unwrap();
        let script = Arc::new(Mutex::new(Script {
            bump_pull,
            conflict: false,
            log: Vec::new(),
        }));
        let shared = script.clone();
        let server =
            serve_requests(move |line, payload| shared.lock().unwrap().respond(line, payload));
        let fixture = Self {
            directory,
            script,
            server,
            cfg: Config::parse(text).unwrap(),
        };
        if saved {
            let mut store = fixture.store();
            store.state.bump_behind.push(BumpBehind {
                id: "acme/web#7".into(),
                package: "Acme.Core".into(),
                from: "1.0.0".into(),
                to: "1.4.0".into(),
                number: 101,
                pull_request_url: "https://github.com/acme/web/pull/101".into(),
                branch: "pipetrain/bump/acme.core-1.4.0".into(),
                recorded_at: "2026-11-18T12:00:00Z".into(),
                publication: None,
            });
            store.save().unwrap();
        }
        fixture
    }

    fn store(&self) -> Store {
        Store::open(Some(self.directory.join("state.json"))).unwrap()
    }

    fn plan(&self, dependent: bool) -> Plan {
        let mut plan = build(
            &self.cfg,
            vec![pr("acme/api", 1, "feat/1", "main")],
            dependent,
        );
        plan.bumps = vec![Need {
            id: "acme/web#7".into(),
            package: "Acme.Core".into(),
            from: "1.0.0".into(),
            to: Some("1.4.0".into()),
            behind: 3,
            limit: 2,
            latest: None,
            queue_published: false,
        }];
        plan
    }

    fn run(&self, plan: &Plan, flag: bool, stop: bool) -> Outcome {
        let api = ApiConfig {
            max_requests: 300,
            max_per_minute: 10000,
            min_remaining: 0,
            max_per_second: 1000,
            ..ApiConfig::default()
        };
        let mut client =
            Client::with_base_url("ghp_FAKE0123456789".into(), &api, false, &self.server.base);
        let mut store = self.store();
        let clock = Mutex::new(Instant::now());
        let slept = Cell::new(Duration::ZERO);
        merge_plan(
            plan,
            &self.cfg,
            &mut client,
            &Settings {
                apply: true,
                gates_confirmed: false,
                allow_questions: false,
                method: None,
                timeout: None,
                feed_registry: None,
                open_pr: false,
                dependency_failure: if stop {
                    DependencyFailure::Stop
                } else {
                    DependencyFailure::Skip
                },
                bump: false,
                bump_options: Default::default(),
                bump_behind: flag.then(Options::default),
            },
            &mut store,
            &mut Hooks {
                confirm: &mut |_| true,
                gate: &mut |_| true,
                sleep: &mut |pause| {
                    *clock.lock().unwrap() += pause;
                    slept.set(slept.get() + pause);
                },
                now: &|| *clock.lock().unwrap(),
                observe: &mut |_| {},
                progress: &mut |_| {},
                runner: None,
            },
        )
        .unwrap()
    }

    fn bump_steps(outcome: &Outcome) -> Vec<(Status, String)> {
        outcome
            .report
            .steps
            .iter()
            .filter(|step| step.kind == Kind::Bump)
            .map(|step| (step.status, step.detail.clone()))
            .collect()
    }

    fn with_conflict(self) -> Self {
        self.script.lock().unwrap().conflict = true;
        self
    }

    fn updates(&self) -> usize {
        self.script
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|line| line.as_str() == "PUT /repos/acme/web/pulls/7/update-branch")
            .count()
    }

    fn touched_bump_pull(&self) -> bool {
        self.script
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|line| line.contains("/repos/acme/web/"))
    }
}

fn build(cfg: &Config, ready: Vec<Pr>, dependent: bool) -> Plan {
    let mut skipped = vec![Skipped {
        pr: pr("acme/web", 7, "feat/7", "main"),
        scope: Scope::Explicit,
        reasons: vec![CAUSE.to_string()],
    }];
    if dependent {
        skipped.push(Skipped {
            pr: pr("acme/app", 9, "feat/9", "main"),
            scope: Scope::Explicit,
            reasons: vec!["depends on acme/web#7, which is waiting".to_string()],
        });
    }
    plan::build(
        &Rules::new(cfg).unwrap(),
        Gathered {
            generated_at: "2026-11-18T12:00:00Z".into(),
            train: "deploy/18-11-2026".into(),
            deploy_branch: "deploy/18-11-2026".into(),
            train_prefix: "deploy/".into(),
            orgs: cfg.orgs.clone(),
            ready: ready
                .into_iter()
                .map(|pr| (pr, "explicit".into()))
                .collect(),
            skipped,
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

#[test]
fn without_the_flag_nothing_runs_for_the_held_pr() {
    let fixture = Fixture::new("open", true);
    let outcome = fixture.run(&fixture.plan(false), false, true);
    assert!(Fixture::bump_steps(&outcome).is_empty());
    assert!(!fixture.touched_bump_pull());
    assert_eq!(outcome.code, 0);
    assert_eq!(fixture.store().state.bump_behind.len(), 1);
}

#[test]
fn an_open_bump_pr_holds_the_dependent_without_opening_another() {
    let fixture = Fixture::new("open", true);
    let first = fixture.run(&fixture.plan(false), true, true);
    let second = fixture.run(&fixture.plan(false), true, true);
    for outcome in [&first, &second] {
        let steps = Fixture::bump_steps(outcome);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].0, Status::Skipped);
        assert!(steps[0].1.contains("waiting for it to be merged"));
        assert_eq!(outcome.code, 0);
        assert!(
            outcome
                .report
                .not_merging
                .iter()
                .any(|held| held.id == "acme/web#7")
        );
    }
    let posts = fixture
        .script
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|line| line.starts_with("POST /repos/"))
        .count();
    assert_eq!(posts, 0);
    assert_eq!(fixture.store().state.bump_behind.len(), 1);
}

#[test]
fn the_independent_pr_still_merges_while_the_bump_waits() {
    let fixture = Fixture::new("open", true);
    let outcome = fixture.run(&fixture.plan(false), true, true);
    let merged: Vec<_> = outcome
        .report
        .steps
        .iter()
        .filter(|step| step.kind == Kind::Merge)
        .collect();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].status, Status::Done);
}

#[test]
fn a_merged_bump_pr_updates_the_held_branch_and_clears_the_state() {
    let fixture = Fixture::new("merged", true);
    let outcome = fixture.run(&fixture.plan(false), true, true);
    let steps = Fixture::bump_steps(&outcome);
    assert_eq!(steps[0].0, Status::Done);
    assert!(steps[0].1.contains("was merged"));
    assert!(steps[0].1.contains("updated the branch"));
    assert_eq!(fixture.updates(), 1);
    assert!(fixture.store().state.bump_behind.is_empty());
    assert_eq!(outcome.code, 0);
}

#[test]
fn an_open_bump_pr_never_updates_the_held_branch() {
    let fixture = Fixture::new("open", true);
    fixture.run(&fixture.plan(false), true, true);
    assert_eq!(fixture.updates(), 0);
}

#[test]
fn a_conflict_updating_the_branch_blocks_the_held_pr_and_the_queue_goes_on() {
    let fixture = Fixture::new("merged", true).with_conflict();
    let outcome = fixture.run(&fixture.plan(false), true, true);
    let steps = Fixture::bump_steps(&outcome);
    assert_eq!(steps[0].0, Status::Skipped);
    assert!(steps[0].1.contains("merge conflict"));
    let held = outcome
        .report
        .not_merging
        .iter()
        .find(|held| held.id == "acme/web#7")
        .unwrap();
    assert_eq!(held.reasons, vec!["merge conflict".to_string()]);
    assert_eq!(held.state, pipetrain::merge::category::Category::Blocked);
    assert!(
        outcome
            .report
            .steps
            .iter()
            .any(|step| step.kind == Kind::Merge && step.status == Status::Done)
    );
    assert!(fixture.store().state.bump_behind.is_empty());
}

#[test]
fn running_again_after_the_merge_updates_again_without_opening_anything() {
    let fixture = Fixture::new("merged", true);
    fixture.run(&fixture.plan(false), true, true);
    let second = fixture.run(&fixture.plan(false), true, true);
    assert_eq!(Fixture::bump_steps(&second)[0].0, Status::Done);
    assert_eq!(fixture.updates(), 2);
    assert!(fixture.store().state.bump_behind.is_empty());
    assert!(
        !fixture
            .script
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|line| line.starts_with("POST /repos/"))
    );
}

#[test]
fn a_failed_bump_stops_when_dependents_need_it_and_skips_otherwise() {
    let stopped = Fixture::new("broken", true);
    let outcome = stopped.run(&stopped.plan(true), true, true);
    assert_eq!(Fixture::bump_steps(&outcome)[0].0, Status::Failed);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome
            .report
            .steps
            .iter()
            .filter(|step| step.kind == Kind::Merge)
            .all(|step| step.status == Status::Skipped)
    );
    let skipping = Fixture::new("broken", true);
    let outcome = skipping.run(&skipping.plan(false), true, false);
    assert_eq!(Fixture::bump_steps(&outcome)[0].0, Status::Failed);
    assert!(
        outcome
            .report
            .steps
            .iter()
            .any(|step| step.kind == Kind::Merge && step.status == Status::Done)
    );
}
