use std::collections::BTreeMap;
use std::process::Command;

mod common;

use common::{Server, serve_handler as serve};
use pipetrain::command::apply::{Kind, Outcome, Status, apply_plan, render};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::discovery::{Scope, Skipped};
use pipetrain::emit;
use pipetrain::github::Client;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Question, Review};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const DEPLOY: &str = "deploy/30-10-2026";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const REF: &str = "GET /repos/acme/api-core/git/ref/heads/master";
const CREATE: &str = "POST /repos/acme/api-core/git/refs";
const FIRST: &str = "PATCH /repos/acme/api-core/pulls/1";
const SECOND: &str = "PATCH /repos/acme/api-core/pulls/2";

fn pr(number: u64, author: &str) -> Pr {
    Pr {
        key: PrKey::new("acme", "api-core", number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/acme/api-core/pull/{number}"),
        body: String::new(),
        head_ref: format!("feat/{number}"),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        author: author.to_string(),
        failed_checks: Vec::new(),
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

fn gathered(prs: Vec<Pr>, skipped: Vec<Skipped>, questions: Vec<Question>) -> (Rules, Gathered) {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let gathered = Gathered {
        generated_at: "2026-10-30T10:00:00-03:00".to_string(),
        train: DEPLOY.to_string(),
        deploy_branch: DEPLOY.to_string(),
        train_prefix: "deploy/".to_string(),
        orgs: cfg.orgs.clone(),
        ready: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        skipped,
        questions,
        external: BTreeMap::new(),
        publishers: Publishers::new(),
        references: Default::default(),
        max_parallel: cfg.max_parallel,
        requests: 0,
        rate_remaining: None,
    };
    (rules, gathered)
}

fn planned(prs: Vec<Pr>) -> Plan {
    let (rules, gathered) = gathered(prs, Vec::new(), Vec::new());
    plan::build(&rules, gathered)
}

fn mixed() -> Plan {
    planned(vec![pr(1, "Alice"), pr(2, "bob")])
}

fn mine() -> Vec<String> {
    vec!["alice".to_string()]
}

fn item<'a>(plan: &'a Plan, id: &str) -> &'a pipetrain::planner::Item {
    plan.items.iter().find(|item| item.id == id).unwrap()
}

#[test]
fn hide_marks_only_other_authors_ignoring_case() {
    let mut plan = planned(vec![pr(1, "Alice"), pr(2, "bob"), pr(3, "")]);
    plan::hide_other_authors(&mut plan, &["ALICE".to_string()]);
    assert!(!item(&plan, "acme/api-core#1").hidden);
    assert!(item(&plan, "acme/api-core#2").hidden);
    assert!(!item(&plan, "acme/api-core#3").hidden);
    assert_eq!(plan.stats.hidden_by_author, 1);
    assert_eq!(plan.shown_authors, Some(vec!["alice".to_string()]));
}

#[test]
fn hide_marks_skipped_prs_of_other_authors() {
    let skipped = vec![
        Skipped {
            pr: pr(7, "bob"),
            scope: Scope::Explicit,
            reasons: vec!["draft".to_string()],
        },
        Skipped {
            pr: pr(8, "alice"),
            scope: Scope::Explicit,
            reasons: vec!["draft".to_string()],
        },
    ];
    let (rules, gathered) = gathered(vec![pr(1, "alice")], skipped, Vec::new());
    let mut plan = plan::build(&rules, gathered);
    plan::hide_other_authors(&mut plan, &mine());
    let hidden: Vec<(&str, bool)> = plan
        .skipped
        .iter()
        .map(|s| (s.id.as_str(), s.hidden))
        .collect();
    assert!(hidden.contains(&("acme/api-core#7", true)));
    assert!(hidden.contains(&("acme/api-core#8", false)));
    assert_eq!(plan.stats.hidden_by_author, 1);
}

#[test]
fn unfiltered_json_has_none_of_the_new_keys() {
    let plan = mixed();
    let text = serde_json::to_string(&plan).unwrap();
    assert!(!text.contains("\"hidden\""));
    assert!(!text.contains("shown_authors"));
    assert!(!text.contains("hidden_by_author"));
}

#[test]
fn filtered_json_marks_hidden_entries() {
    let mut plan = mixed();
    plan::hide_other_authors(&mut plan, &mine());
    let value = serde_json::to_value(&plan).unwrap();
    let items = value["items"].as_array().unwrap();
    let hidden: Vec<&Value> = items.iter().filter(|i| i.get("hidden").is_some()).collect();
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0]["id"], "acme/api-core#2");
    assert_eq!(hidden[0]["hidden"], true);
    assert_eq!(value["shown_authors"], json!(["alice"]));
    assert_eq!(value["stats"]["hidden_by_author"], 1);
}

#[test]
fn filtered_queue_lists_only_visible_merges() {
    let mut plan = mixed();
    plan::hide_other_authors(&mut plan, &mine());
    let lines = emit::queue_lines(&plan);
    let merges: Vec<&String> = lines.iter().filter(|l| l.starts_with("merge:")).collect();
    assert_eq!(merges, vec!["merge:acme/api-core#1"]);
}

#[test]
fn filtered_queue_still_names_a_hidden_publisher() {
    let cfg =
        Config::parse("orgs = [\"acme\"]\n[packages]\n\"@acme/kit\" = \"acme/kit\"\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let mut web = pr(5, "alice");
    web.key = PrKey::new("acme", "web", 5);
    web.head_ref = "feat/a".to_string();
    web.files = vec![pipetrain::model::ChangedFile {
        path: "package.json".to_string(),
        patch: Some("-    \"@acme/kit\": \"^1.0.0\",\n+    \"@acme/kit\": \"^1.1.0\",".to_string()),
    }];
    let mut kit = pr(6, "bob");
    kit.key = PrKey::new("acme", "kit", 6);
    kit.head_ref = "feat/b".to_string();
    let manifests = vec![(
        "acme/kit".to_string(),
        "package.json".to_string(),
        "{\"name\": \"@acme/kit\", \"version\": \"1.1.0\"}".to_string(),
    )];
    let (_, mut gathered) = gathered(vec![web, kit], Vec::new(), Vec::new());
    gathered.publishers = pipetrain::publishers::collect(&manifests);
    gathered.references = pipetrain::publishers::collect_references(&manifests);
    let mut plan = plan::build(&rules, gathered);
    let full = emit::queue_lines(&plan);
    assert!(full.iter().any(|l| l == "merge:acme/kit#6"));
    plan::hide_other_authors(&mut plan, &mine());
    let lines = emit::queue_lines(&plan);
    assert!(lines.iter().any(|l| l == "merge:acme/web#5"));
    assert!(!lines.iter().any(|l| l == "merge:acme/kit#6"));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("# wait:") && l.contains("acme/kit#6"))
    );
}

#[test]
fn hidden_question_does_not_fail_the_exit_code() {
    let key = PrKey::new("acme", "api-core", 2);
    let question = Question::new(Some(&key), "needs-look", "look".to_string(), "do it");
    let (rules, gathered) = gathered(
        vec![pr(1, "alice"), pr(2, "bob")],
        Vec::new(),
        vec![question],
    );
    let mut plan = plan::build(&rules, gathered);
    assert_eq!(emit::exit_code(&plan), 3);
    plan::hide_other_authors(&mut plan, &mine());
    assert!(plan.questions.iter().all(|q| q.hidden));
    assert_eq!(emit::exit_code(&plan), 0);
    assert!(emit::question_lines(&plan).is_empty());
}

#[test]
fn visible_question_still_fails_the_exit_code() {
    let key = PrKey::new("acme", "api-core", 1);
    let question = Question::new(Some(&key), "needs-look", "look".to_string(), "do it");
    let (rules, gathered) = gathered(
        vec![pr(1, "alice"), pr(2, "bob")],
        Vec::new(),
        vec![question],
    );
    let mut plan = plan::build(&rules, gathered);
    plan::hide_other_authors(&mut plan, &mine());
    assert_eq!(emit::exit_code(&plan), 3);
    assert_eq!(emit::question_lines(&plan).len(), 1);
}

#[test]
fn question_without_a_pr_is_never_hidden() {
    let question = Question::new(None, "tree-truncated", "big".to_string(), "pin it");
    let (rules, gathered) = gathered(
        vec![pr(1, "alice"), pr(2, "bob")],
        Vec::new(),
        vec![question],
    );
    let mut plan = plan::build(&rules, gathered);
    plan::hide_other_authors(&mut plan, &mine());
    assert!(plan.questions.iter().all(|q| !q.hidden));
    assert_eq!(emit::exit_code(&plan), 3);
}

fn text(plan: &Plan) -> String {
    anstream::adapter::strip_str(&pipetrain::render::text(plan)).to_string()
}

#[test]
fn render_shows_the_hidden_count_and_omits_hidden_prs() {
    let mut plan = planned(vec![pr(1, "alice"), pr(2, "bob"), pr(3, "carol")]);
    plan.items[1].title = "secret bob work".to_string();
    plan.items[2].title = "secret carol work".to_string();
    plan::hide_other_authors(&mut plan, &mine());
    let shown = text(&plan);
    assert!(shown.contains("hidden    2 PRs by others"), "{shown}");
    assert!(shown.contains("change 1"));
    assert!(!shown.contains("secret bob work"));
    assert!(!shown.contains("secret carol work"));
}

#[test]
fn render_uses_the_singular_for_one_hidden_pr() {
    let mut plan = mixed();
    plan::hide_other_authors(&mut plan, &mine());
    assert!(text(&plan).contains("hidden    1 PR by others"));
}

#[test]
fn render_without_a_filter_has_no_hidden_line() {
    assert!(!text(&mixed()).contains("by others"));
}

#[test]
fn config_parses_authors() {
    let cfg = Config::parse("authors = [\"@me\", \"Alice\"]").unwrap();
    assert_eq!(cfg.authors, vec!["@me".to_string(), "Alice".to_string()]);
}

#[test]
fn config_authors_default_is_empty() {
    assert!(Config::parse("").unwrap().authors.is_empty());
}

#[test]
fn mine_and_all_authors_conflict() {
    let out = Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(["plan", "--mine", "--all-authors"])
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn author_and_all_authors_conflict() {
    let out = Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(["plan", "--author", "a", "--all-authors"])
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn bundle_flags_conflict_too() {
    let out = Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(["bundle", "--mine", "--all-authors"])
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
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

fn client(server: &Server) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url("ghp_FAKE0123456789".to_string(), &cfg, false, &server.base)
}

fn run(server: &Server, plan: &Plan, apply: bool) -> Outcome {
    let mut client = client(server);
    apply_plan(
        plan,
        &mut client,
        apply,
        &mut |_: &str| true,
        &mut |_, _| {},
    )
    .unwrap()
}

#[test]
fn viewer_login_is_lowercase() {
    let server = serve(|_| (200, json!({"data": {"viewer": {"login": "Octocat"}}})));
    let mut client = client(&server);
    assert_eq!(client.viewer_login().unwrap(), "octocat");
    assert_eq!(server.lines(), vec!["POST /graphql".to_string()]);
}

#[test]
fn viewer_login_errors_when_missing() {
    let server = serve(|_| (200, json!({"data": {}})));
    let mut client = client(&server);
    assert!(client.viewer_login().is_err());
}

#[test]
fn bundle_retargets_only_visible_prs() {
    let mut plan = mixed();
    plan::hide_other_authors(&mut plan, &mine());
    let server = serve(happy);
    let outcome = run(&server, &plan, true);
    assert!(!outcome.failed);
    assert_eq!(
        server.lines(),
        vec![REF.to_string(), CREATE.to_string(), FIRST.to_string()]
    );
    assert!(!server.lines().contains(&SECOND.to_string()));
    let actions: Vec<(Kind, &str, Status)> = outcome.report.bundles[0]
        .actions
        .iter()
        .map(|a| (a.kind, a.target.as_str(), a.status))
        .collect();
    assert_eq!(
        actions,
        vec![
            (Kind::CreateBranch, DEPLOY, Status::Done),
            (Kind::Retarget, "acme/api-core#1", Status::Done),
        ]
    );
    let shown = render(&outcome.report);
    assert!(
        shown.contains("1 PR by others left alone: acme/api-core#2"),
        "{shown}"
    );
}

#[test]
fn bundle_without_a_filter_retargets_everything() {
    let plan = mixed();
    let server = serve(happy);
    let outcome = run(&server, &plan, true);
    assert_eq!(
        server.lines(),
        vec![
            REF.to_string(),
            CREATE.to_string(),
            FIRST.to_string(),
            SECOND.to_string()
        ]
    );
    assert!(!render(&outcome.report).contains("by others"));
}

#[test]
fn bundle_with_no_visible_pr_is_dropped() {
    let mut plan = mixed();
    plan::hide_other_authors(&mut plan, &["carol".to_string()]);
    let server = serve(happy);
    let outcome = run(&server, &plan, false);
    assert!(outcome.report.bundles.is_empty());
    assert!(server.lines().is_empty());
    assert_eq!(render(&outcome.report), "nothing to bundle\n");
}
