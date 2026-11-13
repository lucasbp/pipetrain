use std::io::Write;
use std::net::TcpListener;
use std::thread::{self, JoinHandle};

mod common;

use common::read_request;
use pipetrain::config::{ApiConfig, Config};
use pipetrain::discovery::{self, Discovered};
use pipetrain::github::{self, Client};
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::profile::ProfileSet;
use serde_json::{Value, json};

const TRAIN: &str = "deploy/29-10-2026";
const FAKE: &str = "fake-value-for-tests";

fn pr(slug: &str, number: u64, head: &str, base: &str, labels: &[&str]) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
        default_branch: String::new(),
        merge_state_status: String::new(),
        check_details: Vec::new(),
        base_ref: base.to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        author: String::new(),
        failed_checks: Vec::new(),
        mergeable_unknown: false,
        behind: false,
        files: Vec::new(),
        files_complete: true,
        labels: labels.iter().map(ToString::to_string).collect(),
    }
}

fn config(label: Option<&str>) -> Config {
    let mut text = "orgs = [\"acme\"]\n".to_string();
    if let Some(label) = label {
        text.push_str(&format!("train_label = \"{label}\"\n"));
    }
    Config::parse(&text).unwrap()
}

fn discover(prs: Vec<Pr>, label: Option<&str>) -> Discovered {
    discovery::discover(prs, &config(label), TRAIN)
}

fn scopes(found: &Discovered) -> Vec<(String, &'static str)> {
    let mut all: Vec<(String, &'static str)> = found
        .ready
        .iter()
        .map(|(pr, scope)| (pr.key.to_string(), scope.as_str()))
        .chain(
            found
                .skipped
                .iter()
                .map(|s| (s.pr.key.to_string(), s.scope.as_str())),
        )
        .collect();
    all.sort();
    all
}

#[test]
fn a_labelled_pr_on_any_branch_joins_the_train() {
    let found = discover(
        vec![pr("acme/api", 1, "feat/login", "main", &["train"])],
        Some("train"),
    );
    assert_eq!(found.ready.len(), 1);
    assert_eq!(found.ready[0].1.as_str(), "train");
}

#[test]
fn without_a_configured_label_the_same_pr_is_left_out() {
    let found = discover(
        vec![pr("acme/api", 1, "feat/login", "main", &["train"])],
        None,
    );
    assert!(found.ready.is_empty());
    assert!(found.skipped.is_empty());
}

#[test]
fn an_unlabelled_pr_is_left_out_even_when_a_label_is_configured() {
    let found = discover(
        vec![pr("acme/api", 1, "feat/login", "main", &["bug"])],
        Some("train"),
    );
    assert!(found.ready.is_empty());
    assert!(found.skipped.is_empty());
}

#[test]
fn the_label_match_ignores_case() {
    let found = discover(
        vec![
            pr("acme/api", 1, "feat/a", "main", &["TRAIN"]),
            pr("acme/web", 2, "feat/b", "main", &["Release Train"]),
        ],
        Some("release train"),
    );
    assert_eq!(
        scopes(&found),
        vec![("acme/web#2".to_string(), "train")],
        "only the exact label name matches, case aside"
    );
    let found = discover(
        vec![pr("acme/api", 1, "feat/a", "main", &["TRAIN"])],
        Some("train"),
    );
    assert_eq!(found.ready[0].1.as_str(), "train");
}

#[test]
fn a_labelled_pr_that_is_not_ready_is_skipped_as_train() {
    let mut red = pr("acme/api", 1, "feat/a", "main", &["train"]);
    red.checks = Checks::Failure;
    let found = discover(vec![red], Some("train"));
    assert!(found.ready.is_empty());
    assert_eq!(found.skipped.len(), 1);
    assert_eq!(found.skipped[0].scope.as_str(), "train");
}

#[test]
fn the_head_branch_still_wins_without_a_label() {
    let found = discover(vec![pr("acme/api", 1, TRAIN, "main", &[])], Some("train"));
    assert_eq!(found.ready[0].1.as_str(), "train");
}

#[test]
fn a_labelled_pr_with_a_head_shared_across_repos_is_not_a_shared_branch() {
    let found = discover(
        vec![
            pr("acme/api", 1, "feat/shared", "main", &["train"]),
            pr("acme/web", 2, "feat/shared", "main", &[]),
        ],
        Some("train"),
    );
    assert_eq!(scopes(&found), vec![("acme/api#1".to_string(), "train")]);
    assert!(found.questions.is_empty());
}

#[test]
fn the_same_shared_head_is_a_shared_branch_when_nothing_is_labelled() {
    let found = discover(
        vec![
            pr("acme/api", 1, "feat/shared", "main", &[]),
            pr("acme/web", 2, "feat/shared", "main", &[]),
        ],
        Some("train"),
    );
    assert_eq!(
        scopes(&found),
        vec![
            ("acme/api#1".to_string(), "shared-branch"),
            ("acme/web#2".to_string(), "shared-branch"),
        ]
    );
}

#[test]
fn released_gives_a_labelled_pr_the_train_scope() {
    let cfg = config(Some("train"));
    let found = discovery::released(
        vec![
            pr("acme/api", 1, "feat/a", "main", &["train"]),
            pr("acme/web", 2, "feat/b", "main", &[]),
        ],
        &cfg,
        TRAIN,
    );
    assert_eq!(
        scopes(&found),
        vec![
            ("acme/api#1".to_string(), "train"),
            ("acme/web#2".to_string(), "released"),
        ]
    );
}

#[test]
fn released_ignores_labels_when_none_is_configured() {
    let cfg = config(None);
    let found = discovery::released(
        vec![pr("acme/api", 1, "feat/a", "main", &["train"])],
        &cfg,
        TRAIN,
    );
    assert_eq!(found.ready[0].1.as_str(), "released");
}

#[test]
fn an_empty_label_is_a_parse_error() {
    for text in ["train_label = \"\"\n", "train_label = \"  \"\n"] {
        let message = format!("{:#}", ProfileSet::parse(text).unwrap_err());
        assert!(message.contains("`train_label` is empty"), "{message}");
    }
}

#[test]
fn an_empty_label_inside_a_profile_names_the_profile() {
    let text = "[profile.acme]\norgs = [\"acme\"]\ntrain_label = \"\"\n";
    let message = format!("{:#}", ProfileSet::parse(text).unwrap_err());
    assert!(message.contains("profile `acme`"), "{message}");
    assert!(message.contains("`train_label` is empty"), "{message}");
}

#[test]
fn a_profile_inherits_the_shared_label_and_may_override_it() {
    let text = "train_label = \"train\"\n[profile.acme]\norgs = [\"acme\"]\n[profile.globex]\norgs = [\"globex\"]\ntrain_label = \"ship\"\n";
    let set = ProfileSet::parse(text).unwrap();
    assert_eq!(
        set.find("acme").unwrap().config.train_label.as_deref(),
        Some("train")
    );
    assert_eq!(
        set.find("globex").unwrap().config.train_label.as_deref(),
        Some("ship")
    );
}

#[test]
fn the_label_defaults_to_none() {
    assert_eq!(config(None).train_label, None);
}

fn search_node(number: u64, labels: Value) -> Value {
    json!({
        "number": number,
        "title": "change",
        "url": format!("https://github.com/acme/api/pull/{number}"),
        "headRefName": "feat/a",
        "baseRefName": "main",
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "labels": labels,
    })
}

#[test]
fn parsing_reads_the_label_names() {
    let mut bare = search_node(3, Value::Null);
    bare.as_object_mut().unwrap().remove("labels");
    let nodes = vec![
        search_node(
            1,
            json!({ "nodes": [{ "name": "train" }, { "name": "bug" }] }),
        ),
        search_node(2, json!({ "nodes": [] })),
        bare,
    ];
    let response = json!({ "data": { "search": { "nodes": nodes } } });
    let prs = github::parse_search(&response);
    assert_eq!(prs.len(), 3);
    assert_eq!(prs[0].labels, vec!["train".to_string(), "bug".to_string()]);
    assert!(prs[0].has_label("TRAIN"));
    assert!(!prs[0].has_label("release"));
    assert!(prs[1].labels.is_empty());
    assert!(prs[2].labels.is_empty());
}

fn serve_once(body: String) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(reply.as_bytes()).unwrap();
        request
    });
    (base, handle)
}

#[test]
fn the_search_query_asks_for_labels_and_the_reply_fills_them() {
    let body = json!({ "data": { "search": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [search_node(9, json!({ "nodes": [{ "name": "train" }] }))],
    } } })
    .to_string();
    let (base, server) = serve_once(body);
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    let mut client = Client::with_base_url(FAKE.to_string(), &cfg, false, &base);
    let prs = client.open_prs("acme").unwrap();
    let request = server.join().unwrap();
    assert!(request.contains("labels(first: 50) { nodes { name } }"));
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].labels, vec!["train".to_string()]);
}
