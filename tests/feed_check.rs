use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use pipetrain::command::feed::{Checker, annotate};
use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::{self, Publishers, References};
use serde_json::{Value, json};

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn lines(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if read == 0 || text.contains("\r\n\r\n") {
            return text;
        }
    }
}

fn serve<H>(make: impl FnOnce(String) -> H) -> Server
where
    H: Fn(&str) -> (u16, Value) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handler = make(base.clone());
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
            record.lock().unwrap().push(line);
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

fn packument(versions: &[&str]) -> Value {
    let map: serde_json::Map<String, Value> = versions
        .iter()
        .map(|version| ((*version).to_string(), json!({})))
        .collect();
    json!({"name": "@acme/kit", "versions": map})
}

fn npm_server(versions: &'static [&'static str]) -> Server {
    serve(move |_| {
        move |line: &str| {
            if line == "GET /@acme%2fkit" {
                (200, packument(versions))
            } else {
                (404, json!({}))
            }
        }
    })
}

fn nuget_server(versions: &'static [&'static str]) -> Server {
    serve(move |base| {
        move |line: &str| match line {
            "GET /v3/index.json" => (
                200,
                json!({
                    "version": "3.0.0",
                    "resources": [
                        {"@id": format!("{base}/download/"), "@type": "PackageBaseAddress/3.0.0"}
                    ]
                }),
            ),
            "GET /download/cmutil/index.json" => (200, json!({"versions": versions})),
            _ => (404, json!({})),
        }
    })
}

fn status_server(status: u16) -> Server {
    serve(move |_| move |_: &str| (status, json!({})))
}

fn closed_base() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

fn pr(slug: &str, number: u64, head: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
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
    }
}

fn bump(mut consumer: Pr, packages: &[&str]) -> Pr {
    let patch: Vec<String> = packages
        .iter()
        .map(|p| format!("-    \"{p}\": \"^1.0.0\",\n+    \"{p}\": \"^1.1.0\","))
        .collect();
    consumer.files = vec![ChangedFile {
        path: "package.json".to_string(),
        patch: Some(patch.join("\n")),
    }];
    consumer
}

fn bump_patch(mut consumer: Pr, path: &str, patch: &str) -> Pr {
    consumer.files = vec![ChangedFile {
        path: path.to_string(),
        patch: Some(patch.to_string()),
    }];
    consumer
}

fn planned(extra: &str, prs: Vec<Pr>) -> Plan {
    let cfg = Config::parse(&format!("orgs = [\"acme\"]\n{extra}\n")).unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let manifests: Vec<(String, String, String)> = Vec::new();
    let publishers: Publishers = publishers::collect(&manifests);
    let references: References = publishers::collect_references(&manifests);
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: prs
                .into_iter()
                .map(|p| (p, "explicit".to_string()))
                .collect(),
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers,
            references,
            max_parallel: 4,
            requests: 0,
            rate_remaining: None,
        },
    )
}

const NPM_EXTRA: &str = "[packages]\n\"@acme/kit\" = \"acme/kit\"\n";
const NUGET_EXTRA: &str = "[packages]\n\"cmutil\" = \"acme/cmutil\"\n";

fn npm_plan() -> Plan {
    planned(
        NPM_EXTRA,
        vec![
            bump(pr("acme/web", 5, "feat/a"), &["@acme/kit"]),
            pr("acme/kit", 6, "feat/b"),
        ],
    )
}

fn nuget_plan(version: &str) -> Plan {
    let patch = format!(
        "-    <PackageReference Include=\"CmUtil\" Version=\"1.2610.3022.39\" />\n+    <PackageReference Include=\"CmUtil\" Version=\"{version}\" />"
    );
    planned(
        NUGET_EXTRA,
        vec![
            bump_patch(pr("acme/api", 5, "feat/a"), "src/Api/Api.csproj", &patch),
            pr("acme/cmutil", 6, "feat/b"),
        ],
    )
}

fn waits(plan: &Plan) -> Vec<String> {
    emit::queue_lines(plan)
        .into_iter()
        .filter(|l| l.starts_with("# wait"))
        .collect()
}

fn merges(plan: &Plan) -> Vec<String> {
    emit::queue_lines(plan)
        .into_iter()
        .filter(|l| l.starts_with("merge:"))
        .collect()
}

const NPM_WAIT: &str =
    "# wait: package @acme/kit 1.1.0 from acme/kit#6 must be published before acme/web#5";

fn after_of(plan: &Plan, id: &str) -> Value {
    let items = serde_json::to_value(&plan.items).unwrap();
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap()["after"][0]
        .clone()
}

#[test]
fn the_suffix_covers_each_outcome() {
    assert_eq!(emit::feed_suffix(Some(true), None), " (published)");
    assert_eq!(emit::feed_suffix(Some(false), None), " (not yet)");
    assert_eq!(
        emit::feed_suffix(None, Some("boom")),
        " (feed check failed: boom)"
    );
    assert_eq!(emit::feed_suffix(None, None), "");
}

#[test]
fn a_set_outcome_shows_on_the_versioned_wait_line() {
    let mut built = npm_plan();
    built.items.iter_mut().for_each(|item| {
        item.after
            .iter_mut()
            .for_each(|dep| dep.published = Some(true));
    });
    assert_eq!(waits(&built), vec![format!("{NPM_WAIT} (published)")]);
    built.items.iter_mut().for_each(|item| {
        item.after.iter_mut().for_each(|dep| {
            dep.published = None;
            dep.feed_error = Some("cannot reach host".to_string());
        });
    });
    assert_eq!(
        waits(&built),
        vec![format!("{NPM_WAIT} (feed check failed: cannot reach host)")]
    );
}

#[test]
fn a_versionless_wait_never_gets_a_suffix() {
    let mut built = nuget_plan("$(CmUtilVersion)");
    built.items.iter_mut().for_each(|item| {
        item.after
            .iter_mut()
            .for_each(|dep| dep.published = Some(true));
    });
    assert_eq!(
        waits(&built),
        vec!["# wait: package cmutil from acme/cmutil#6 must be published before acme/api#5"]
    );
}

#[test]
fn without_the_check_nothing_is_added_to_the_queue_or_the_json() {
    let built = npm_plan();
    assert_eq!(waits(&built), vec![NPM_WAIT.to_string()]);
    let json = serde_json::to_string(&built).unwrap();
    assert!(!json.contains("\"published\""));
    assert!(!json.contains("feed_error"));
    assert!(!json.contains("feed_checks"));
}

#[test]
fn a_published_npm_version_is_marked_published() {
    let server = npm_server(&["1.0.0", "1.1.0"]);
    let mut built = npm_plan();
    let before = merges(&built);
    let mut checker = Checker::with_registry(Some(server.base.clone()));
    assert_eq!(annotate(&mut built, &mut checker), 1);
    assert_eq!(waits(&built), vec![format!("{NPM_WAIT} (published)")]);
    assert_eq!(merges(&built), before);
    assert_eq!(server.lines(), vec!["GET /@acme%2fkit".to_string()]);
    assert_eq!(after_of(&built, "acme/web#5")["published"], json!(true));
    assert!(after_of(&built, "acme/web#5").get("feed_error").is_none());
}

#[test]
fn an_npm_version_the_feed_lacks_is_marked_not_yet() {
    let server = npm_server(&["1.0.0"]);
    let mut built = npm_plan();
    let mut checker = Checker::with_registry(Some(server.base.clone()));
    annotate(&mut built, &mut checker);
    assert_eq!(waits(&built), vec![format!("{NPM_WAIT} (not yet)")]);
    assert_eq!(after_of(&built, "acme/web#5")["published"], json!(false));
}

#[test]
fn a_nuget_version_is_checked_through_the_service_index() {
    let server = nuget_server(&["1.2610.3022.39", "1.2610.3022.40"]);
    let mut built = nuget_plan("1.2610.3022.40");
    let mut checker = Checker::with_registry(Some(format!("{}/v3/index.json", server.base)));
    assert_eq!(annotate(&mut built, &mut checker), 1);
    assert_eq!(
        waits(&built),
        vec![
            "# wait: package cmutil 1.2610.3022.40 from acme/cmutil#6 must be published before acme/api#5 (published)"
        ]
    );
    assert_eq!(
        server.lines(),
        vec![
            "GET /v3/index.json".to_string(),
            "GET /download/cmutil/index.json".to_string()
        ]
    );
}

#[test]
fn a_nuget_version_the_feed_lacks_is_marked_not_yet() {
    let server = nuget_server(&["1.2610.3022.39"]);
    let mut built = nuget_plan("1.2610.3022.40");
    let mut checker = Checker::with_registry(Some(format!("{}/v3/index.json", server.base)));
    annotate(&mut built, &mut checker);
    assert_eq!(
        waits(&built),
        vec![
            "# wait: package cmutil 1.2610.3022.40 from acme/cmutil#6 must be published before acme/api#5 (not yet)"
        ]
    );
}

#[test]
fn a_refused_feed_says_it_needs_read_packages() {
    for status in [401, 403] {
        let server = status_server(status);
        let mut built = npm_plan();
        let before = merges(&built);
        let mut checker = Checker::with_registry(Some(server.base.clone()));
        annotate(&mut built, &mut checker);
        assert_eq!(
            waits(&built),
            vec![format!(
                "{NPM_WAIT} (feed check failed: the feed refused the request (401/403): needs read:packages)"
            )]
        );
        assert_eq!(merges(&built), before);
        let dep = after_of(&built, "acme/web#5");
        assert!(dep.get("published").is_none());
        assert!(
            dep["feed_error"]
                .as_str()
                .unwrap()
                .contains("read:packages")
        );
    }
}

#[test]
fn a_missing_package_reports_a_failed_check() {
    let server = status_server(404);
    let mut built = npm_plan();
    let mut checker = Checker::with_registry(Some(server.base.clone()));
    annotate(&mut built, &mut checker);
    let lines = waits(&built);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains("(feed check failed: package not found in"),
        "{}",
        lines[0]
    );
}

#[test]
fn an_unreachable_feed_reports_a_failed_check_on_one_line() {
    let mut built = npm_plan();
    let mut checker = Checker::with_registry(Some(closed_base()));
    annotate(&mut built, &mut checker);
    let lines = waits(&built);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with(&format!("{NPM_WAIT} (feed check failed: cannot reach")),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with(')'));
}

#[test]
fn one_lookup_serves_every_consumer_of_the_same_version() {
    let server = npm_server(&["1.1.0"]);
    let mut built = planned(
        NPM_EXTRA,
        vec![
            bump(pr("acme/web", 5, "feat/a"), &["@acme/kit"]),
            bump(pr("acme/app", 7, "feat/c"), &["@acme/kit"]),
            pr("acme/kit", 6, "feat/b"),
        ],
    );
    let mut checker = Checker::with_registry(Some(server.base.clone()));
    assert_eq!(annotate(&mut built, &mut checker), 1);
    assert_eq!(annotate(&mut built, &mut checker), 1);
    assert_eq!(server.lines().len(), 1);
    let lines = waits(&built);
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.ends_with("(published)")));
}

#[test]
fn a_plan_without_versioned_waits_makes_no_request() {
    let server = npm_server(&["1.1.0"]);
    let mut built = nuget_plan("$(CmUtilVersion)");
    let mut checker = Checker::with_registry(Some(server.base.clone()));
    assert_eq!(annotate(&mut built, &mut checker), 0);
    assert!(server.lines().is_empty());
}
