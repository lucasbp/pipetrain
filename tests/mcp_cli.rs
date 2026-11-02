use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pipetrain-mcp-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fake {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn requests(&self) -> Vec<String> {
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

fn pr_node() -> Value {
    json!({
        "number": 7,
        "title": "Add a thing",
        "url": "https://example.test/acme/api/pull/7",
        "body": "",
        "headRefName": "feature/thing",
        "baseRefName": "main",
        "baseRef": { "target": { "oid": "b".repeat(40) } },
        "headRefOid": "a".repeat(40),
        "isDraft": false,
        "state": "OPEN",
        "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE",
        "author": { "login": "octocat" },
        "labels": { "nodes": [] },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "latestOpinionatedReviews": { "nodes": [{ "state": "APPROVED" }] },
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": {
            "state": "SUCCESS", "contexts": { "nodes": [] }
        } } }] }
    })
}

fn answer(line: &str, body: &str) -> (u16, Value) {
    if line.starts_with("POST /graphql") {
        if body.contains("pullRequest(number") {
            return (
                200,
                json!({ "data": { "repository": { "pullRequest": pr_node() } } }),
            );
        }
        if body.contains("compare(") {
            let ahead =
                json!({ "ref": { "compare": { "aheadBy": 1, "behindBy": 0, "status": "ahead" } } });
            return (200, json!({ "data": { "t0": ahead, "t1": ahead } }));
        }
        if body.contains("viewer") {
            return (200, json!({ "data": { "viewer": { "login": "octocat" } } }));
        }
        return (404, json!({ "message": "not faked" }));
    }
    if line.starts_with("GET /rate_limit") {
        return (
            200,
            json!({ "resources": { "core": { "remaining": 4900, "limit": 5000, "reset": 4_000_000_000_i64 } } }),
        );
    }
    if line.contains("/pulls/7/files") {
        return (200, json!([]));
    }
    if line.starts_with("GET /@acme%2fui") {
        return (200, json!({ "versions": { "1.0.0": {}, "1.2.0": {} } }));
    }
    (404, json!({ "message": "not faked" }))
}

fn serve() -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let request = read_request(&mut stream);
            let line = request.lines().next().unwrap_or_default().to_string();
            let body = request.split_once("\r\n\r\n").map_or("", |(_, rest)| rest);
            let (status, reply) = answer(&line, body);
            record.lock().unwrap().push(request.clone());
            let text = reply.to_string();
            let wire = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(wire.as_bytes());
        }
    });
    Fake { base, seen }
}

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
    raw: Vec<String>,
    log_dir: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(fake: &Fake, extra_env: &[(&str, &str)]) -> Session {
    let home = scratch("home");
    let config = home.join("config.toml");
    fs::write(&config, "orgs = [\"acme\"]\n").unwrap();
    let log_dir = scratch("log");
    let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
    command
        .args(["mcp", "--config"])
        .arg(&config)
        .env("GH_TOKEN", FAKE)
        .env("PIPETRAIN_API_URL", &fake.base)
        .env("PIPETRAIN_CACHE_DIR", scratch("cache"))
        .env("PIPETRAIN_LOG_DIR", &log_dir)
        .env("PATH", scratch("empty-path"))
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    Session {
        child,
        stdin: Some(stdin),
        stdout,
        next: 1,
        raw: Vec::new(),
        log_dir,
    }
}

impl Session {
    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).unwrap();
        assert!(read > 0, "the server closed stdout");
        self.raw.push(line.clone());
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("stdout line is not JSON ({error}): {line}"))
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        );
        let answer = self.read();
        assert_eq!(answer["jsonrpc"], "2.0");
        assert_eq!(answer["id"], id);
        answer
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    fn handshake(&mut self) {
        let answer = self.call(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
        );
        assert_eq!(answer["result"]["protocolVersion"], "2025-06-18");
        self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    }
}

fn first_text(answer: &Value) -> String {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn initialize_and_tools_list_expose_five_read_only_tools() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    let answer = session.call(
        "initialize",
        json!({ "protocolVersion": "2024-11-05", "capabilities": {} }),
    );
    assert_eq!(answer["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(answer["result"]["serverInfo"]["name"], "pipetrain");
    assert!(answer["result"]["capabilities"]["tools"].is_object());
    session.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    let listed = session.call("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["plan", "feed", "bump", "doctor", "order_suggest"]);
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["description"].as_str().unwrap().len() < 160);
    }
    for forbidden in ["bundle", "apply", "update", "init", "write"] {
        assert!(
            !names.iter().any(|name| name.contains(forbidden)),
            "{forbidden}"
        );
    }
    let bump = tools.iter().find(|tool| tool["name"] == "bump").unwrap();
    assert!(bump["inputSchema"]["properties"].get("apply").is_none());
}

#[test]
fn ping_answers_an_empty_result() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    assert_eq!(session.call("ping", json!({}))["result"], json!({}));
}

#[test]
fn doctor_returns_the_schema_1_json_of_the_command() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    let answer = session.tool("doctor", json!({ "org": [] }));
    assert_eq!(answer["result"]["isError"], false);
    let report: Value = serde_json::from_str(&first_text(&answer)).unwrap();
    assert_eq!(report["schema"], 1);
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["id"] == "api" && check["status"] == "ok")
    );
}

#[test]
fn plan_returns_the_schema_1_json_for_an_explicit_pr() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    let answer = session.tool("plan", json!({ "prs": ["acme/api#7"] }));
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let plan: Value = serde_json::from_str(&first_text(&answer)).unwrap();
    assert_eq!(plan["schema"], 1);
    let items = plan["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "acme/api#7");
    for request in fake.requests() {
        assert!(!request.contains("mutation"));
        assert!(request.starts_with("GET ") || request.starts_with("POST /graphql"));
    }
}

#[test]
fn plan_rejects_all_together_with_prs_as_a_tool_error() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    let answer = session.tool("plan", json!({ "prs": ["acme/api#7"], "all": true }));
    assert_eq!(answer["result"]["isError"], true);
    assert!(first_text(&answer).contains("invalid arguments"));
    assert!(fake.requests().is_empty());
}

#[test]
fn feed_reads_a_fake_registry_and_returns_the_command_json() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    let answer = session.tool(
        "feed",
        json!({ "package": "@acme/ui", "version": "1.2.0", "registry": fake.base }),
    );
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let report: Value = serde_json::from_str(&first_text(&answer)).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["published"], true);
}

#[test]
fn a_command_failure_is_a_tool_error_and_the_server_keeps_going() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    let answer = session.tool("plan", json!({ "prs": ["acme/api#999999x"] }));
    assert_eq!(answer["result"]["isError"], true);
    let again = session.call("ping", json!({}));
    assert_eq!(again["result"], json!({}));
}

#[test]
fn protocol_errors_use_the_json_rpc_codes_and_stdout_stays_clean() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();

    let unknown = session.tool("bundle", json!({ "apply": true }));
    assert_eq!(unknown["error"]["code"], -32602);

    let missing = session.call("resources/list", json!({}));
    assert_eq!(missing["error"]["code"], -32601);

    session.send("{this is not json");
    let broken = session.read();
    assert_eq!(broken["error"]["code"], -32700);
    assert_eq!(broken["id"], Value::Null);

    session.send("");
    session.send("   ");
    let after_blank = session.call("ping", json!({}));
    assert_eq!(after_blank["result"], json!({}));

    for line in &session.raw {
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
    }
}

#[test]
fn the_token_never_appears_in_any_answer() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    session.tool("doctor", json!({}));
    session.tool("plan", json!({ "prs": ["acme/api#7"] }));
    session.tool(
        "feed",
        json!({ "package": "@acme/ui", "registry": fake.base }),
    );
    session.tool("order_suggest", json!({ "repos": ["acme/api"] }));
    session.tool("plan", json!({ "prs": ["acme/api#7"], "org": [FAKE] }));
    session.call("tools/list", json!({}));
    assert!(!session.raw.is_empty());
    for line in &session.raw {
        assert!(!line.contains(FAKE), "{line}");
    }
    assert!(
        fake.requests().iter().any(|request| request.contains(FAKE)),
        "the fake API never saw the token, so the check above proves nothing"
    );
}

#[test]
fn bump_failures_are_tool_errors_and_nothing_is_written() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    let answer = session.tool(
        "bump",
        json!({ "package": "@acme/ui", "to": "1.2.0", "repos": ["acme/api"] }),
    );
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    for request in fake.requests() {
        assert!(request.starts_with("GET ") || request.starts_with("POST /graphql"));
    }
}

#[test]
fn each_tool_call_is_logged_only_when_the_log_is_on() {
    let fake = serve();
    let mut off = start(&fake, &[]);
    off.handshake();
    off.tool("doctor", json!({}));
    assert!(!off.log_dir.join("log.jsonl").exists());

    let mut on = start(&fake, &[("PIPETRAIN_LOG", "1")]);
    on.handshake();
    on.tool("doctor", json!({}));
    on.tool("plan", json!({ "prs": ["acme/api#7"] }));
    let text = fs::read_to_string(on.log_dir.join("log.jsonl")).unwrap();
    let commands: Vec<String> = text
        .lines()
        .map(|line| {
            let value: Value = serde_json::from_str(line).unwrap();
            value["command"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(commands, ["mcp:doctor", "mcp:plan"]);
    assert!(!text.contains("acme"));
    assert!(!text.contains(FAKE));
}

#[test]
fn closing_stdin_ends_the_server_with_success() {
    let fake = serve();
    let mut session = start(&fake, &[]);
    session.handshake();
    session.stdin.take();
    assert!(session.child.wait().unwrap().success());
}
