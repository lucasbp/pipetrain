use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

mod common;

use common::read_head as read_request;
use serde_json::{Value, json};

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn lines(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(" HTTP/1.1")
                    .to_string()
            })
            .collect()
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

fn feed(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .arg("feed")
        .args(args)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn packument(versions: &[&str]) -> Value {
    let map: serde_json::Map<String, Value> = versions
        .iter()
        .map(|version| ((*version).to_string(), json!({})))
        .collect();
    json!({"name": "pkg", "versions": map})
}

#[test]
fn public_registries_exclude_yanked_files_and_order_versions_by_ecosystem() {
    for (ecosystem, package, metadata, latest, missing) in [
        ("cargo", "abc", [
            r#"{"name":"abc","vers":"1.0.0+original","yanked":false}"#,
            r#"{"name":"abc","vers":"2.0.0","yanked":true}"#,
            r#"{"name":"abc","vers":"1.0.0-rc.1","yanked":false}"#,
        ].join("\n"), "1.0.0+original","1.0.0+different"),
        ("pypi","Test_Util", r#"{"releases":{"1.0rc2":[{"yanked":false}],"1.0.post1":[{"yanked":false}],"1.0.dev12":[{"yanked":false}],"1.0rc11":[{"yanked":false}],"2!1":[{"yanked":true}],"2.0":[]}}"#.into(), "1.0.post1","2!1"),
    ] {
        let (base, requests) = common::serve_bodies(vec![metadata.clone(),metadata]);
        let output = feed(&[package,"--ecosystem",ecosystem,"--registry",&base,"--json"]);
        assert!(output.status.success(),"{}",stderr(&output));
        let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(doc["latest"],latest);
        let output = feed(&[package,"--ecosystem",ecosystem,"--registry",&base,"--version",missing,"--wait","--timeout","0","--json"]);
        assert_eq!(output.status.code(),Some(1),"{}",stderr(&output));
        let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(doc["published"],false);
        assert_eq!(doc["timed_out"],true);
        assert_eq!(requests.join().unwrap().len(),2);
    }
}

#[test]
fn go_proxy_verifies_an_unlisted_exact_version_and_rejects_a_mismatched_answer() {
    let wanted = "v0.0.0-20260101000000-abcdefabcdef";
    for (returned, code) in [(wanted, 0), ("v1.0.0", 3)] {
        let (base, requests) = common::serve_bodies(vec![
            "v1.2.0\n".into(),
            json!({"Version":returned}).to_string(),
        ]);
        let output = feed(&[
            "example.org/Module",
            "--ecosystem",
            "go",
            "--registry",
            &base,
            "--version",
            wanted,
            "--wait",
            "--timeout",
            "0",
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
        let requests = requests.join().unwrap();
        assert!(requests[0].starts_with("GET /example.org/!module/@v/list "));
        assert!(requests[1].starts_with(&format!("GET /example.org/!module/@v/{wanted}.info ")));
    }
}

#[test]
fn a_go_module_without_tags_uses_the_proxy_latest_and_a_missing_module_is_not_found() {
    for (first, second, code) in [
        (
            common::canned("200 OK", ""),
            common::canned(
                "200 OK",
                r#"{"Version":"v0.0.0-20260101000000-abcdefabcdef"}"#,
            ),
            0,
        ),
        (
            common::canned("410 Gone", ""),
            common::canned("404 Not Found", ""),
            1,
        ),
    ] {
        let (base, requests) = common::serve(vec![first, second]);
        let output = feed(&[
            "example.org/module",
            "--ecosystem",
            "go",
            "--registry",
            &base,
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
        if code == 1 {
            assert!(output.stdout.is_empty());
        }
        assert!(requests.join().unwrap()[1].starts_with("GET /example.org/module/@latest "));
    }
}

#[test]
fn npm_version_that_is_published() {
    let server = serve(|_| |_: &str| (200, packument(&["1.0.0", "1.1.0", "1.2.0"])));
    let output = feed(&[
        "left-pad",
        "--version",
        "1.1.0",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "left-pad 1.1.0: published");
    assert_eq!(server.lines(), vec!["GET /left-pad".to_string()]);
    let request = server.requests()[0].to_lowercase();
    assert!(request.contains("accept: application/vnd.npm.install-v1+json"));
    assert!(!request.contains("authorization"));
}

#[test]
fn npm_scoped_package_is_encoded_in_the_path() {
    let server = serve(|_| |_: &str| (200, packument(&["2.0.0"])));
    let output = feed(&["@acme/ui", "--version", "2.0.0", "--registry", &server.base]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "@acme/ui 2.0.0: published");
    assert_eq!(server.lines(), vec!["GET /@acme%2fui".to_string()]);
}

#[test]
fn npm_version_that_is_not_published() {
    let server = serve(|_| |_: &str| (200, packument(&["1.0.0", "1.9.0", "1.10.0"])));
    let output = feed(&[
        "left-pad",
        "--version",
        "2.0.0",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "left-pad 2.0.0: not published (latest 1.10.0)"
    );
}

#[test]
fn npm_without_a_version_lists_the_latest_and_the_recent_ones() {
    let server = serve(|_| {
        |_: &str| {
            (
                200,
                packument(&[
                    "1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0", "1.5.0", "1.6.0",
                ]),
            )
        }
    });
    let output = feed(&["left-pad", "--ecosystem", "npm", "--registry", &server.base]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "left-pad: latest 1.6.0\nlast 5: 1.6.0, 1.5.0, 1.4.0, 1.3.0, 1.2.0"
    );
}

#[test]
fn nuget_version_is_found_through_the_service_index() {
    let server = serve(|base| {
        move |line: &str| {
            if line == "GET /v3/index.json" {
                (
                    200,
                    json!({
                        "version": "3.0.0",
                        "resources": [
                            {"@id": format!("{base}/query"), "@type": "SearchQueryService"},
                            {"@id": format!("{base}/flat/"), "@type": "PackageBaseAddress/3.0.0"}
                        ]
                    }),
                )
            } else if line == "GET /flat/acme.util/index.json" {
                (
                    200,
                    json!({"versions": ["1.2610.2314.49", "1.2610.2314.5"]}),
                )
            } else {
                (404, json!({}))
            }
        }
    });
    let index = format!("{}/v3/index.json", server.base);
    let output = feed(&[
        "Acme.Util",
        "--version",
        "1.2610.2314.49",
        "--registry",
        &index,
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Acme.Util 1.2610.2314.49: published");
    assert_eq!(
        server.lines(),
        vec![
            "GET /v3/index.json".to_string(),
            "GET /flat/acme.util/index.json".to_string()
        ]
    );
    assert!(
        !server.requests()[1]
            .to_lowercase()
            .contains("authorization")
    );
}

#[test]
fn nuget_requires_an_org_without_a_registry() {
    let output = feed(&["Acme.Util", "--version", "1.0.0"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--org is required for nuget"));
}

#[test]
fn missing_package_reports_not_found() {
    let server = serve(|_| |_: &str| (404, json!({"error": "Not found"})));
    let output = feed(&["ghost", "--ecosystem", "npm", "--registry", &server.base]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        format!("package not found in {}", server.base)
    );
}

#[test]
fn refused_request_explains_the_read_packages_scope() {
    let server = serve(|_| |_: &str| (401, json!({"error": "Unauthorized"})));
    let output = feed(&["left-pad", "--ecosystem", "npm", "--registry", &server.base]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("read:packages"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "");
}

#[test]
fn forbidden_request_also_exits_two() {
    let server = serve(|_| |_: &str| (403, json!({})));
    let output = feed(&["left-pad", "--ecosystem", "npm", "--registry", &server.base]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("read:packages"));
}

#[test]
fn json_output_has_the_documented_shape() {
    let server = serve(|_| |_: &str| (200, packument(&["1.0.0", "1.1.0"])));
    let output = feed(&[
        "left-pad",
        "--version",
        "1.1.0",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        parsed,
        json!({
            "schema": 1,
            "package": "left-pad",
            "ecosystem": "npm",
            "feed": server.base,
            "version": "1.1.0",
            "published": true,
            "latest": "1.1.0",
            "versions": ["1.1.0", "1.0.0"]
        })
    );
}

#[test]
fn json_output_without_a_version_leaves_published_null() {
    let server = serve(|_| |_: &str| (200, packument(&["1.0.0"])));
    let output = feed(&[
        "left-pad",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed["version"], Value::Null);
    assert_eq!(parsed["published"], Value::Null);
    assert_eq!(parsed["latest"], "1.0.0");
}
