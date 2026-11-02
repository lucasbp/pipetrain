use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pipetrain::command::apply::{
    Gate, Kind, Outcome, Running, Status, apply_plan, gate, render, render_with,
};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::github::Client;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const DEPLOY: &str = "deploy/30-10-2026";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const REF: &str = "GET /repos/acme/api-core/git/ref/heads/master";
const CREATE: &str = "POST /repos/acme/api-core/git/refs";
const FIRST: &str = "PATCH /repos/acme/api-core/pulls/1";
const SECOND: &str = "PATCH /repos/acme/api-core/pulls/2";
const STOPPED: &str = "stopped after an earlier failure";

fn pr(number: u64) -> Pr {
    Pr {
        key: PrKey::new("acme", "api-core", number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("github.com/acme/api-core/pull/{number}"),
        body: String::new(),
        head_ref: format!("feat/{number}"),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        author: String::new(),
        failed_checks: Vec::new(),
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

fn two_prs() -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T10:00:00-03:00".to_string(),
            train: DEPLOY.to_string(),
            deploy_branch: DEPLOY.to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: cfg.orgs.clone(),
            ready: vec![
                (pr(1), "explicit".to_string()),
                (pr(2), "explicit".to_string()),
            ],
            skipped: Vec::new(),
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

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn lines(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                let first = request.lines().next().unwrap_or_default();
                first.trim_end_matches(" HTTP/1.1").to_string()
            })
            .collect()
    }

    fn body(&self, line: &str) -> Value {
        let seen = self.seen.lock().unwrap();
        let request = seen
            .iter()
            .find(|request| request.starts_with(&format!("{line} ")))
            .unwrap();
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap()
    }

    fn writes(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| !line.starts_with("GET "))
            .collect()
    }
}

fn serve(handler: impl Fn(&str) -> (u16, Value) + Send + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read_request(&mut stream);
            let line = request
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end_matches(" HTTP/1.1")
                .to_string();
            let (status, body) = handler(&line);
            record.lock().unwrap().push(request);
            let body = body.to_string();
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    Server { base, seen }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if read == 0 || body_complete(&text) {
            return text;
        }
    }
}

fn body_complete(text: &str) -> bool {
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let wanted = head
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    body.len() >= wanted
}

fn happy(line: &str) -> (u16, Value) {
    if line.starts_with("GET ") {
        (200, json!({"object": {"sha": SHA}}))
    } else if line.starts_with("POST ") {
        (201, json!({}))
    } else {
        (200, json!({}))
    }
}

fn fail_on(
    target: &'static str,
    status: u16,
    message: &'static str,
) -> impl Fn(&str) -> (u16, Value) + Send + 'static {
    move |line| {
        if line == target {
            (status, json!({"message": message}))
        } else {
            happy(line)
        }
    }
}

fn client(server: &Server) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url("ghp_FAKE0123456789".to_string(), &cfg, false, &server.base)
}

fn run(server: &Server, plan: &Plan, apply: bool, answer: bool) -> Outcome {
    let mut client = client(server);
    apply_plan(
        plan,
        &mut client,
        apply,
        &mut |_: &str| answer,
        &mut |_, _| {},
    )
    .unwrap()
}

fn actions(outcome: &Outcome) -> Vec<(Kind, &str, Status, &str)> {
    outcome.report.bundles[0]
        .actions
        .iter()
        .map(|a| (a.kind, a.target.as_str(), a.status, a.detail.as_str()))
        .collect()
}

#[test]
fn a_dry_run_only_reads_the_base_ref() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), false, true);

    assert_eq!(server.lines(), vec![REF]);
    assert!(!outcome.report.applied);
    assert!(!outcome.failed && !outcome.aborted);
    assert!(
        outcome.report.bundles[0]
            .actions
            .iter()
            .all(|a| a.status == Status::Planned)
    );
    let text = render(&outcome.report);
    assert!(text.starts_with("dry run, nothing was written"));
    assert!(text.contains(&format!("create branch {DEPLOY} from master (0123456)")));
    assert!(text.contains(&format!("retarget acme/api-core#1: master -> {DEPLOY}")));
    assert!(text.contains(&format!("retarget acme/api-core#2: master -> {DEPLOY}")));
}

#[test]
fn apply_creates_the_branch_then_retargets_each_pr_in_order() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, true);

    assert_eq!(server.lines(), vec![REF, CREATE, FIRST, SECOND]);
    assert_eq!(
        server.body(CREATE),
        json!({"ref": format!("refs/heads/{DEPLOY}"), "sha": SHA})
    );
    assert_eq!(server.body(FIRST), json!({"base": DEPLOY}));
    assert_eq!(server.body(SECOND), json!({"base": DEPLOY}));
    assert!(outcome.report.applied);
    assert!(!outcome.failed && !outcome.aborted);
    assert_eq!(
        actions(&outcome),
        vec![
            (Kind::CreateBranch, DEPLOY, Status::Done, ""),
            (Kind::Retarget, "acme/api-core#1", Status::Done, ""),
            (Kind::Retarget, "acme/api-core#2", Status::Done, ""),
        ]
    );
    assert!(!render(&outcome.report).contains("dry run"));
}

#[test]
fn declining_the_confirmation_writes_nothing() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, false);

    assert!(outcome.aborted);
    assert!(!outcome.failed);
    assert!(!outcome.report.applied);
    assert!(server.writes().is_empty());
}

#[test]
fn an_existing_branch_is_skipped_and_the_retargets_still_run() {
    let server = serve(fail_on(CREATE, 422, "Reference already exists"));
    let outcome = run(&server, &two_prs(), true, true);

    assert_eq!(server.lines(), vec![REF, CREATE, FIRST, SECOND]);
    assert!(!outcome.failed);
    assert_eq!(
        actions(&outcome),
        vec![
            (
                Kind::CreateBranch,
                DEPLOY,
                Status::Skipped,
                "branch already exists"
            ),
            (Kind::Retarget, "acme/api-core#1", Status::Done, ""),
            (Kind::Retarget, "acme/api-core#2", Status::Done, ""),
        ]
    );
}

fn assert_stops_at_first_patch(status: u16, message: &'static str) {
    let server = serve(fail_on(FIRST, status, message));
    let outcome = run(&server, &two_prs(), true, true);

    assert_eq!(server.lines(), vec![REF, CREATE, FIRST]);
    assert!(outcome.failed);
    assert!(outcome.report.applied);
    let found = actions(&outcome);
    assert_eq!(found[0].2, Status::Done);
    assert_eq!(found[1].2, Status::Failed);
    assert!(found[1].3.contains(message), "detail: {}", found[1].3);
    assert_eq!(
        found[2],
        (Kind::Retarget, "acme/api-core#2", Status::Skipped, STOPPED)
    );
}

#[test]
fn a_422_on_a_retarget_stops_the_rest() {
    assert_stops_at_first_patch(422, "Validation Failed");
}

#[test]
fn a_403_on_a_retarget_stops_the_rest() {
    assert_stops_at_first_patch(403, "Resource not accessible by integration");
}

type Seen = Vec<(Running, Vec<Status>)>;

fn observed(server: &Server, apply: bool) -> (Outcome, Seen) {
    let mut client = client(server);
    let mut seen: Seen = Vec::new();
    let outcome = apply_plan(
        &two_prs(),
        &mut client,
        apply,
        &mut |_: &str| true,
        &mut |report, running| {
            let statuses = report.bundles[0].actions.iter().map(|a| a.status).collect();
            seen.push((running, statuses));
        },
    )
    .unwrap();
    (outcome, seen)
}

#[test]
fn the_observer_sees_every_step_before_and_after_it_runs() {
    use Status::{Done, Planned};
    let server = serve(happy);
    let (_, seen) = observed(&server, true);

    assert_eq!(
        seen,
        vec![
            (None, vec![Planned, Planned, Planned]),
            (Some((0, 0)), vec![Planned, Planned, Planned]),
            (None, vec![Done, Planned, Planned]),
            (Some((0, 1)), vec![Done, Planned, Planned]),
            (None, vec![Done, Done, Planned]),
            (Some((0, 2)), vec![Done, Done, Planned]),
            (None, vec![Done, Done, Done]),
        ]
    );
}

#[test]
fn the_observer_sees_the_rest_skipped_after_a_failure() {
    use Status::{Done, Failed, Planned, Skipped};
    let server = serve(fail_on(FIRST, 422, "Validation Failed"));
    let (outcome, seen) = observed(&server, true);

    assert!(outcome.failed);
    assert_eq!(
        seen,
        vec![
            (None, vec![Planned, Planned, Planned]),
            (Some((0, 0)), vec![Planned, Planned, Planned]),
            (None, vec![Done, Planned, Planned]),
            (Some((0, 1)), vec![Done, Planned, Planned]),
            (None, vec![Done, Failed, Skipped]),
        ]
    );
}

#[test]
fn the_observer_is_not_called_with_a_step_on_a_dry_run() {
    let server = serve(happy);
    let (_, seen) = observed(&server, false);

    assert!(seen.iter().all(|(running, _)| running.is_none()));
}

#[test]
fn a_pr_already_on_the_branch_is_skipped_without_a_request() {
    let mut plan = two_prs();
    plan.items
        .iter_mut()
        .find(|item| item.id == "acme/api-core#1")
        .unwrap()
        .base = DEPLOY.to_string();
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert_eq!(server.lines(), vec![REF, CREATE, SECOND]);
    assert!(!outcome.failed);
    assert_eq!(
        actions(&outcome)[1],
        (
            Kind::Retarget,
            "acme/api-core#1",
            Status::Skipped,
            format!("already based on {DEPLOY}").as_str()
        )
    );
    assert_eq!(actions(&outcome)[2].2, Status::Done);
}

#[test]
fn an_existing_train_pr_means_no_read_and_no_branch_creation() {
    let mut plan = two_prs();
    plan.bundles[0].existing_train_pr = Some("acme/api-core#9".to_string());
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert_eq!(server.lines(), vec![FIRST, SECOND]);
    assert!(
        actions(&outcome)
            .iter()
            .all(|a| a.0 == Kind::Retarget && a.2 == Status::Done)
    );
    assert!(render(&outcome.report).contains(&format!(
        "branch exists: {DEPLOY} is the head of acme/api-core#9"
    )));
}

fn assert_refused(branch: &str) {
    let mut plan = two_prs();
    plan.bundles[0].branch = branch.to_string();
    let server = serve(happy);
    let mut client = client(&server);
    let error = apply_plan(
        &plan,
        &mut client,
        true,
        &mut |_: &str| true,
        &mut |_, _| {},
    )
    .unwrap_err();

    assert!(error.to_string().contains("refusing to bundle"));
    assert!(server.writes().is_empty());
}

#[test]
fn a_branch_equal_to_the_base_is_refused() {
    assert_refused("master");
}

#[test]
fn an_empty_branch_is_refused() {
    assert_refused("");
}

#[test]
fn the_json_report_has_the_documented_shape() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, true);
    let value = serde_json::to_value(&outcome.report).unwrap();

    assert_eq!(value["schema"], 1);
    assert_eq!(value["applied"], true);
    let bundle = &value["bundles"][0];
    assert_eq!(bundle["repo"], "acme/api-core");
    assert_eq!(bundle["base"], "master");
    assert_eq!(bundle["branch"], DEPLOY);
    assert!(bundle.get("note").is_none());
    let list = bundle["actions"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["kind"], "create-branch");
    assert_eq!(list[0]["target"], DEPLOY);
    assert_eq!(list[1]["kind"], "retarget");
    assert_eq!(list[1]["target"], "acme/api-core#1");
    for action in list {
        assert_eq!(action["status"], "done");
        assert_eq!(action["detail"], "");
        assert!(action.get("step").is_none());
        assert!(action.get("note").is_none());
    }
}

#[test]
fn no_bundles_renders_nothing_to_bundle() {
    let mut plan = two_prs();
    plan.bundles.clear();
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert!(server.lines().is_empty());
    assert_eq!(render(&outcome.report), "nothing to bundle\n");
}

#[test]
fn the_gate_follows_its_truth_table() {
    assert_eq!(gate(false, false, true, None), Gate::DryRun);
    assert_eq!(gate(false, true, false, None), Gate::DryRun);
    assert_eq!(gate(true, true, false, None), Gate::Proceed);
    assert_eq!(gate(true, false, false, None), Gate::Refuse);
    assert_eq!(gate(true, false, true, None), Gate::Ask);
    assert_eq!(gate(true, false, true, Some("yes")), Gate::Proceed);
    assert_eq!(gate(true, false, true, Some(" yes \n")), Gate::Proceed);
    assert_eq!(gate(true, false, true, Some("y")), Gate::Abort);
    assert_eq!(gate(true, false, true, Some("")), Gate::Abort);
    assert_eq!(gate(true, false, true, Some("no")), Gate::Abort);
}

fn mixed_report() -> Outcome {
    let server = serve(happy);
    let mut outcome = run(&server, &two_prs(), true, true);
    let extra = run(&server, &two_prs(), true, true)
        .report
        .bundles
        .remove(0)
        .actions
        .pop()
        .unwrap();
    let list = &mut outcome.report.bundles[0].actions;
    list.push(extra);
    list[1].status = Status::Skipped;
    list[2].status = Status::Failed;
    outcome
}

#[test]
fn the_summary_counts_the_actions_by_status() {
    let outcome = mixed_report();
    let text = render(&outcome.report);

    assert!(
        text.ends_with("\n\n2 done, 1 skipped, 1 failed\n"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn the_summary_ends_with_the_elapsed_time_when_given() {
    let outcome = mixed_report();
    let text = render_with(&outcome.report, false, Some(Duration::from_millis(1500)));

    assert!(
        text.ends_with("2 done, 1 skipped, 1 failed in 1.5 s\n"),
        "{text}"
    );
}

#[test]
fn a_dry_run_has_no_summary() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), false, true);
    let text = render_with(&outcome.report, false, Some(Duration::from_millis(10)));

    assert!(!text.contains(" done"), "{text}");
    assert!(!text.contains("skipped,"), "{text}");
}

#[test]
fn the_plain_render_has_no_escape_codes_and_matches_the_default() {
    let outcome = mixed_report();

    assert_eq!(
        render_with(&outcome.report, false, None),
        render(&outcome.report)
    );
}

#[test]
fn the_colored_dry_run_banner_is_yellow() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), false, true);
    let text = render_with(&outcome.report, true, None);

    assert!(
        text.starts_with(
            "\u{1b}[33mdry run, nothing was written; pass --apply to do it\u{1b}[0m\n\n"
        ),
        "{text}"
    );
    assert!(text.contains("\u{1b}[33mcreate branch\u{1b}[0m"), "{text}");
    assert!(
        text.contains(&format!("\u{1b}[36m{DEPLOY}\u{1b}[0m")),
        "{text}"
    );
    assert!(text.contains("\u{1b}[1macme/api-core#1\u{1b}[0m"), "{text}");
}

#[test]
fn the_colored_statuses_use_green_dim_and_red() {
    let outcome = mixed_report();
    let text = render_with(&outcome.report, true, None);

    assert!(text.contains("\u{1b}[32mdone:\u{1b}[0m"), "{text}");
    assert!(text.contains("\u{1b}[2mskipped:\u{1b}[0m"), "{text}");
    assert!(text.contains("\u{1b}[1;31mfailed:\u{1b}[0m"), "{text}");
    assert!(
        text.contains(
            "\u{1b}[32m2 done\u{1b}[0m, \u{1b}[2m1 skipped\u{1b}[0m, \u{1b}[1;31m1 failed\u{1b}[0m"
        ),
        "{text}"
    );
}

#[test]
fn zero_failures_are_dim() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, true);
    let text = render_with(&outcome.report, true, None);

    assert!(text.contains("\u{1b}[2m0 failed\u{1b}[0m"), "{text}");
}

#[test]
fn the_colored_nothing_to_bundle_is_dim() {
    let mut plan = two_prs();
    plan.bundles.clear();
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert_eq!(
        render_with(&outcome.report, true, None),
        "\u{1b}[2mnothing to bundle\u{1b}[0m\n"
    );
}
