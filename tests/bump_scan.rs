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
        max_per_second: 1000,
        ..ApiConfig::default()
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
fn restricting_a_scanned_report_marks_the_other_levels() {
    let repos = vec![RepoRefs {
        repo: "acme/api".to_string(),
        refs: vec![
            ("Major.csproj".to_string(), "1.0.0".to_string()),
            ("Minor.csproj".to_string(), "2.0.0".to_string()),
            ("Patch.csproj".to_string(), "2.1.0".to_string()),
            ("Current.csproj".to_string(), "2.1.4".to_string()),
        ],
    }];
    let report = Report::build("Acme.Util", Ecosystem::Nuget, "2.1.4", &repos)
        .restrict(&[bump::Level::Patch]);
    let statuses: Vec<Status> = report.repos[0].files.iter().map(|row| row.status).collect();
    assert_eq!(
        statuses,
        vec![
            Status::OtherLevel,
            Status::OtherLevel,
            Status::Change,
            Status::UpToDate,
        ]
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

#[test]
fn manifest_pins_share_the_scan_and_apply_rules() {
    let cases = [
        (
            Ecosystem::Cargo,
            "demo",
            "Cargo.toml",
            "[package]\nname='demo'\nversion='1.0.0'\n[dependencies]\ndemo='=1.0.0'\n[dev-dependencies]\ndemo='1.0.0'\n[target.'cfg(unix)'.dependencies.renamed]\npackage='demo'\nversion='= 1.8.0'\n",
            "=1.0.0",
            "2.0.0",
            2,
        ),
        (
            Ecosystem::Pypi,
            "Demo_Pkg",
            "pyproject.toml",
            "[project]\nname='demo-pkg'\nversion='1.0.0'\ndependencies=['Demo.Pkg[env] (==1.0.0) ; python_version < \"4\"', 'demo_pkg>=1.0.0']\n[tool.poetry.group.dev.dependencies]\ndemo-pkg={version='1.8.0',optional=true}\n",
            "==1.0.0",
            "2.0.0rc1",
            2,
        ),
        (
            Ecosystem::Go,
            "example.org/Demo",
            "go.mod",
            "module example.org/user\r\nrequire (\r\n\texample.org/Demo v1.0.0 // indirect\r\n\texample.org/demo v1.0.0\r\n)\r\n",
            "v1.0.0",
            "v1.2.0",
            1,
        ),
    ];
    for (ecosystem, package, file, manifest, current, target, changes) in cases {
        let repo = "acme/client";
        let server = serve([(repo.to_owned(), files(&[(file, manifest)]))].into());
        let scan = bump::scan(&mut client(&server), &names(&[repo]), package, ecosystem).unwrap();
        let report = Report::build(package, ecosystem, target, &scan.repos);
        assert_eq!(report.count(Status::Change), changes);
        let pinned =
            bump::pin_manifest(ecosystem, manifest, package, target, &[current.to_owned()])
                .unwrap();
        let expected = match ecosystem {
            Ecosystem::Cargo => manifest.replace("demo='=1.0.0'", "demo='=2.0.0'"),
            Ecosystem::Pypi => manifest.replace("(==1.0.0)", "(==2.0.0rc1)"),
            Ecosystem::Go => manifest.replace("Demo v1.0.0", "Demo v1.2.0"),
            _ => unreachable!(),
        };
        assert_eq!(pinned, expected);
        assert!(
            bump::pin_manifest(ecosystem, &pinned, package, target, &[current.to_owned()]).is_err()
        );
    }
}

#[test]
fn indirect_sources_and_ambiguous_versions_require_a_manual_choice() {
    let cases = [
        (
            Ecosystem::Cargo,
            "demo",
            "2.0.0",
            "Cargo.toml",
            "[dependencies]\ndemo='=1.0.0'\n[patch.crates-io]\ndemo={path='../demo'}",
        ),
        (
            Ecosystem::Cargo,
            "demo",
            "2.0.0",
            "Cargo.toml",
            "[dependencies]\ndemo={version='=1.0.0', registry='private'}",
        ),
        (
            Ecosystem::Pypi,
            "demo",
            "2.0.0",
            "pyproject.toml",
            "[project]\ndependencies=['demo==1.0.0']\n[tool.uv.sources]\ndemo={workspace=true}",
        ),
        (
            Ecosystem::Pypi,
            "demo",
            "2.0.0",
            "pyproject.toml",
            "[project]\ndependencies=['demo @ file:///local', 'demo==1.*','demo>=1,<2']",
        ),
        (
            Ecosystem::Go,
            "example.org/d",
            "v1.1.0",
            "go.mod",
            "require example.org/d v1.0.0\nreplace (\nexample.org/d => ./d\n)",
        ),
        (
            Ecosystem::Go,
            "example.org/d",
            "v1.1.0",
            "go.mod",
            "require example.org/d v1.0.0\nexclude example.org/d v1.1.0",
        ),
        (
            Ecosystem::Go,
            "example.org/d",
            "v2.0.0",
            "go.mod",
            "require example.org/d v1.0.0",
        ),
        (
            Ecosystem::Go,
            "example.org/d/v2",
            "v2.1.0",
            "go.mod",
            "require example.org/d/v2 v2.0.0-20260101010000-abcdef123456",
        ),
    ];
    for (ecosystem, package, target, path, body) in cases {
        let manifests = vec![("acme/client".into(), path.into(), body.into())];
        let refs = bump::refs_of(&manifests, package, ecosystem);
        assert!(!refs.is_empty(), "{body}");
        let repo = RepoRefs {
            repo: "acme/client".into(),
            refs,
        };
        let report = Report::build(package, ecosystem, target, &[repo]);
        assert_eq!(
            report.count(Status::Range),
            report.repos[0].files.len(),
            "{body}"
        );
    }
}

#[test]
fn duplicate_pins_are_rechecked_and_pep440_levels_follow_release_components() {
    let text = "[project]\ndependencies=['demo==1.0rc1','demo==1.0rc1','demo==1.1']";
    let refs = bump::refs_of(
        &[("acme/web".into(), "pyproject.toml".into(), text.into())],
        "demo",
        Ecosystem::Pypi,
    );
    let repo = RepoRefs {
        repo: "acme/web".into(),
        refs,
    };
    let report = Report::build("demo", Ecosystem::Pypi, "1.1.post1", &[repo])
        .restrict(&[bump::Level::Minor]);
    assert_eq!(report.count(Status::Change), 2);
    assert_eq!(report.count(Status::OtherLevel), 1);
    let pin = "==1.0rc1".to_owned();
    assert!(
        bump::pin_manifest(
            Ecosystem::Pypi,
            text,
            "demo",
            "1.1.post1",
            std::slice::from_ref(&pin)
        )
        .is_err()
    );
    let updated = bump::pin_manifest(
        Ecosystem::Pypi,
        text,
        "demo",
        "1.1.post1",
        &[pin.clone(), pin],
    )
    .unwrap();
    assert_eq!(updated, text.replace("1.0rc1", "1.1.post1"));
}

#[test]
fn workspace_sources_affect_only_the_selected_package_and_members() {
    let cargo =
        "[workspace]\nmembers=['crates/*']\n[patch.crates-io]\nexample={path='vendor/example'}";
    let python = "[tool.uv.workspace]\nmembers=['crates/*']\nexclude=['crates/isolated']\n[tool.uv.sources]\nexample={workspace=true}";
    for (eco, file, root, member) in [
        (
            Ecosystem::Cargo,
            "Cargo.toml",
            cargo,
            "[dependencies]\nexample='=1.0.0'",
        ),
        (
            Ecosystem::Pypi,
            "pyproject.toml",
            python,
            "[project]\ndependencies=['example==1.0.0']",
        ),
    ] {
        let path = format!("crates/child/{file}");
        let tree = vec![file.to_owned(), path.clone()];
        let read = |p: &str| {
            assert_eq!(p, file);
            Ok(root.to_owned())
        };
        assert!(bump::check_workspace_sources(eco, "example", &path, member, &tree, read).is_err());
        assert!(bump::check_workspace_sources(eco, "other", &path, member, &tree, read).is_ok());
        if eco == Ecosystem::Pypi {
            assert!(
                bump::check_workspace_sources(
                    eco,
                    "example",
                    "crates/isolated/pyproject.toml",
                    member,
                    &tree,
                    read
                )
                .is_ok()
            );
        }
        let refs = bump::refs_of(
            &[
                ("acme/app".into(), file.into(), root.into()),
                ("acme/app".into(), path.clone(), member.into()),
            ],
            "example",
            eco,
        );
        assert!(
            refs.iter()
                .any(|(p, s)| p == &path && s.starts_with("manual:"))
        );
    }
}
