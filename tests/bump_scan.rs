use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use pipetrain::command::bump::{self, RepoRefs, Report, Status};
use pipetrain::command::order::names_of;
use pipetrain::config::ApiConfig;
use pipetrain::feed::Ecosystem;
use pipetrain::github::Client;
use serde_json::{Value, json};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

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
}

type Fixtures = BTreeMap<String, BTreeMap<String, String>>;

fn serve(fixtures: Fixtures) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);
            let line = request.lines().next().unwrap_or_default().to_string();
            let body = request
                .split_once("\r\n\r\n")
                .and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
                .unwrap_or(Value::Null);
            log.lock().unwrap().push(request);
            let (status, answer) = answer(&fixtures, &line, &body);
            let text = answer.to_string();
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Server { base, seen }
}

fn answer(fixtures: &Fixtures, line: &str, body: &Value) -> (u16, Value) {
    if line.starts_with("GET ") && line.contains("/git/trees/") {
        let repo = fixtures
            .iter()
            .find(|(slug, _)| line.contains(slug.as_str()));
        return match repo {
            Some((_, files)) => {
                let tree: Vec<Value> = files
                    .keys()
                    .map(|path| json!({"type": "blob", "path": path}))
                    .collect();
                (200, json!({"tree": tree, "truncated": false}))
            }
            None => (404, json!({"message": "Not Found"})),
        };
    }
    if line.starts_with("POST ") {
        let query = body["query"].as_str().unwrap_or_default();
        let variables = &body["variables"];
        let slug = format!(
            "{}/{}",
            variables["o"].as_str().unwrap_or_default(),
            variables["r"].as_str().unwrap_or_default()
        );
        let Some(files) = fixtures.get(&slug) else {
            return (200, json!({"data": {"repository": null}}));
        };
        if query.contains("defaultBranchRef") {
            return (
                200,
                json!({"data": {"repository": {"defaultBranchRef": {"target": {"oid": SHA}}}}}),
            );
        }
        let mut fields = serde_json::Map::new();
        let mut index = 0;
        while let Some(expression) = variables[format!("e{index}")].as_str() {
            let path = expression
                .split_once(':')
                .map(|(_, path)| path)
                .unwrap_or_default();
            let text = files.get(path).cloned().unwrap_or_default();
            fields.insert(format!("f{index}"), json!({"text": text}));
            index += 1;
        }
        return (200, json!({"data": {"repository": fields}}));
    }
    (404, json!({"message": "Not Found"}))
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut text = String::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap_or(0);
        text.push_str(&String::from_utf8_lossy(&chunk[..read]));
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

fn client(server: &Server) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url("ghp_FAKE0123456789".to_string(), &cfg, false, &server.base)
}

fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(path, text)| (path.to_string(), text.to_string()))
        .collect()
}

fn csproj(version: &str) -> String {
    format!(
        r#"<Project><ItemGroup><PackageReference Include="Acme.Util" Version="{version}" /></ItemGroup></Project>"#
    )
}

fn fixtures() -> Fixtures {
    let mut all = Fixtures::new();
    all.insert(
        "acme/api".to_string(),
        files(&[
            ("src/Api/Api.csproj", &csproj("1.2.0")),
            ("src/Worker/Worker.csproj", &csproj("2.0.0")),
            (
                "web/package.json",
                r#"{"dependencies":{"acme.util":"1.0.0"}}"#,
            ),
            ("README.md", "Acme.Util"),
        ]),
    );
    all.insert(
        "acme/other".to_string(),
        files(&[(
            "Other.csproj",
            r#"<Project><PackageReference Include="Acme.Other" Version="1.0.0" /></Project>"#,
        )]),
    );
    all
}

fn names(repos: &[&str]) -> BTreeMap<String, String> {
    names_of(repos.iter().map(|repo| repo.to_string()))
}

#[test]
fn a_dry_run_reports_each_manifest_and_only_reads() {
    let server = serve(fixtures());
    let mut client = client(&server);
    let scan = bump::scan(
        &mut client,
        &names(&["acme/api", "acme/other"]),
        "Acme.Util",
        Ecosystem::Nuget,
    )
    .unwrap();
    assert_eq!(scan.repos.len(), 1);
    assert_eq!(scan.repos[0].repo, "acme/api");
    let report = Report::build("Acme.Util", Ecosystem::Nuget, "2.0.0", &scan.repos);
    let statuses: Vec<(String, Status)> = report.repos[0]
        .files
        .iter()
        .map(|row| (row.path.clone(), row.status))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("src/Api/Api.csproj".to_string(), Status::Change),
            ("src/Worker/Worker.csproj".to_string(), Status::UpToDate),
        ]
    );
    let text = bump::render_with(&report, false, None);
    assert!(text.contains("src/Api/Api.csproj"));
    assert!(text.contains("1.2.0 -> 2.0.0"));
    assert!(text.contains("up to date"));
    assert!(text.contains("1 would change, 1 up to date, 0 left alone in 1 repo"));
    assert!(text.trim_end().ends_with("dry run: nothing was written"));
    let json = bump::to_json(&report);
    assert_eq!(json["dry_run"], json!(true));
    assert_eq!(json["repos"][0]["files"][0]["status"], json!("change"));
    assert!(
        server
            .lines()
            .iter()
            .all(|line| { line.starts_with("GET ") || line.starts_with("POST /graphql") })
    );
}

#[test]
fn only_manifests_of_the_chosen_ecosystem_are_read() {
    let server = serve(fixtures());
    let mut client = client(&server);
    let scan = bump::scan(
        &mut client,
        &names(&["acme/api"]),
        "acme.util",
        Ecosystem::Npm,
    )
    .unwrap();
    assert_eq!(
        scan.repos,
        vec![RepoRefs {
            repo: "acme/api".to_string(),
            refs: vec![("web/package.json".to_string(), "1.0.0".to_string())],
        }]
    );
}

#[test]
fn a_package_referenced_nowhere_gives_an_empty_scan() {
    let server = serve(fixtures());
    let mut client = client(&server);
    let scan = bump::scan(
        &mut client,
        &names(&["acme/api", "acme/other"]),
        "Missing.Package",
        Ecosystem::Nuget,
    )
    .unwrap();
    assert!(scan.repos.is_empty());
    assert!(scan.warnings.is_empty());
}

#[test]
fn an_unknown_repo_is_skipped_with_a_warning_and_the_rest_is_read() {
    let server = serve(fixtures());
    let mut client = client(&server);
    let scan = bump::scan(
        &mut client,
        &names(&["acme/api", "acme/gone"]),
        "Acme.Util",
        Ecosystem::Nuget,
    )
    .unwrap();
    assert_eq!(scan.repos.len(), 1);
    assert!(scan.warnings.iter().any(|w| w.contains("acme/gone")));
}

#[test]
fn when_no_repo_can_be_read_the_scan_fails() {
    let server = serve(fixtures());
    let mut client = client(&server);
    let result = bump::scan(
        &mut client,
        &names(&["acme/gone"]),
        "Acme.Util",
        Ecosystem::Nuget,
    );
    let error = result.unwrap_err().to_string();
    assert!(error.contains("no repo could be read"), "{error}");
}
