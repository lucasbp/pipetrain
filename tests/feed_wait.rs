use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn serve<H>(handler: H) -> Server
where
    H: Fn(usize) -> (u16, String) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read_request(&mut stream);
            let index = {
                let mut seen = record.lock().unwrap();
                seen.push(request);
                seen.len()
            };
            let (status, body) = handler(index);
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
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
        if read == 0 || text.contains("\r\n\r\n") {
            return text;
        }
    }
}

fn packument(versions: &[&str]) -> String {
    let map: serde_json::Map<String, Value> = versions
        .iter()
        .map(|version| ((*version).to_string(), json!({})))
        .collect();
    json!({"name": "pkg", "versions": map}).to_string()
}

fn run(server: &Server, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(["feed", "acme-util", "--ecosystem", "npm", "--registry"])
        .arg(&server.base)
        .args(extra)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap()
}

fn bare(args: &[&str]) -> Output {
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

#[test]
fn wait_requires_a_version() {
    let output = bare(&["acme-util", "--ecosystem", "npm", "--wait"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--wait requires --version"));
    assert_eq!(stdout(&output), "");
}

#[test]
fn zero_interval_is_rejected() {
    let server = serve(|_| (200, packument(&["1.0.0"])));
    let output = run(
        &server,
        &["--version", "1.0.0", "--wait", "--interval", "0"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--interval must be at least 1 second"));
    assert_eq!(server.count(), 0);
}

#[test]
fn wait_options_without_wait_are_rejected() {
    let server = serve(|_| (200, packument(&["1.0.0"])));
    for flag in [["--interval", "5"], ["--timeout", "5"]] {
        let output = run(&server, &["--version", "1.0.0", flag[0], flag[1]]);
        assert_eq!(output.status.code(), Some(1), "{}", flag[0]);
        assert!(
            stderr(&output).contains("need --wait"),
            "{}",
            stderr(&output)
        );
    }
    let output = run(&server, &["--version", "1.0.0", "--no-progress"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(server.count(), 0);
}

#[test]
fn published_version_returns_at_once() {
    let server = serve(|_| (200, packument(&["1.0.0", "1.1.0"])));
    let output = run(&server, &["--version", "1.1.0", "--wait"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "acme-util 1.1.0: published");
    assert_eq!(server.count(), 1);
}

#[test]
fn waits_until_the_version_appears() {
    let server = serve(|index| {
        if index < 3 {
            (200, packument(&["1.0.0"]))
        } else {
            (200, packument(&["1.0.0", "1.1.0"]))
        }
    });
    let output = run(
        &server,
        &[
            "--version",
            "1.1.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "acme-util 1.1.0: published");
    assert_eq!(server.count(), 3);
    let progress = stderr(&output);
    assert!(
        progress.contains("acme-util 1.1.0: not published yet (attempt 1)"),
        "{progress}"
    );
    assert!(progress.contains("attempt 2"), "{progress}");
}

#[test]
fn times_out_with_exit_one_and_the_latest_version() {
    let server = serve(|_| (200, packument(&["1.0.0"])));
    let output = run(
        &server,
        &[
            "--version",
            "2.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "2",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "acme-util 2.0.0: not published after 2s (latest 1.0.0)"
    );
    assert_eq!(server.count(), 3);
}

#[test]
fn times_out_without_latest_when_the_feed_has_no_versions() {
    let server = serve(|_| (200, packument(&[])));
    let output = run(
        &server,
        &[
            "--version",
            "2.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "1",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "acme-util 2.0.0: not published after 1s");
}

#[test]
fn timeout_zero_checks_once() {
    let server = serve(|_| (200, packument(&["1.0.0"])));
    let output = run(&server, &["--version", "2.0.0", "--wait", "--timeout", "0"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "acme-util 2.0.0: not published after 0s (latest 1.0.0)"
    );
    assert_eq!(server.count(), 1);
}

#[test]
fn unauthorized_aborts_without_retrying() {
    let server = serve(|_| (401, json!({"error": "Unauthorized"}).to_string()));
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("read:packages"));
    assert_eq!(stdout(&output), "");
    assert_eq!(server.count(), 1);
}

#[test]
fn forbidden_aborts_without_retrying() {
    let server = serve(|_| (403, json!({}).to_string()));
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(server.count(), 1);
}

#[test]
fn a_missing_package_counts_as_not_yet_published() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let server = serve(move |_| {
        if counter.fetch_add(1, Ordering::SeqCst) < 2 {
            (404, json!({"error": "Not found"}).to_string())
        } else {
            (200, packument(&["1.0.0"]))
        }
    });
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "acme-util 1.0.0: published");
    assert_eq!(server.count(), 3);
}

#[test]
fn a_package_that_never_appears_times_out_with_exit_one() {
    let server = serve(|_| (404, json!({"error": "Not found"}).to_string()));
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "1",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "acme-util 1.0.0: not published after 1s");
}

#[test]
fn transient_errors_are_retried_until_published() {
    let server = serve(|index| {
        if index < 3 {
            (500, json!({"error": "boom"}).to_string())
        } else {
            (200, packument(&["1.0.0"]))
        }
    });
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "acme-util 1.0.0: published");
    assert_eq!(server.count(), 3);
    let progress = stderr(&output);
    assert!(progress.contains("failed"), "{progress}");
    assert!(progress.contains("trying again in"), "{progress}");
}

#[test]
fn a_persistent_server_error_exits_three_with_empty_stdout() {
    let server = serve(|_| (500, json!({"error": "boom"}).to_string()));
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "2",
        ],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).contains("gave up after"),
        "{}",
        stderr(&output)
    );
    assert!(server.count() >= 2);
}

#[test]
fn invalid_json_at_the_timeout_exits_three() {
    let server = serve(|_| (200, "not json".to_string()));
    let output = run(
        &server,
        &[
            "--version",
            "1.0.0",
            "--wait",
            "--interval",
            "1",
            "--timeout",
            "1",
        ],
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
}

#[test]
fn json_adds_the_wait_fields_when_published() {
    let server = serve(|_| (200, packument(&["1.0.0", "1.1.0"])));
    let output = run(&server, &["--version", "1.1.0", "--wait", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        parsed,
        json!({
            "schema": 1,
            "package": "acme-util",
            "ecosystem": "npm",
            "feed": server.base,
            "version": "1.1.0",
            "published": true,
            "latest": "1.1.0",
            "versions": ["1.1.0", "1.0.0"],
            "waited": true,
            "attempts": 1,
            "elapsed_seconds": 0,
            "timed_out": false
        })
    );
}

#[test]
fn json_marks_a_timeout() {
    let server = serve(|_| (200, packument(&["1.0.0"])));
    let output = run(
        &server,
        &[
            "--version",
            "2.0.0",
            "--wait",
            "--json",
            "--interval",
            "1",
            "--timeout",
            "1",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed["schema"], 1);
    assert_eq!(parsed["published"], false);
    assert_eq!(parsed["waited"], true);
    assert_eq!(parsed["timed_out"], true);
    assert_eq!(parsed["attempts"], 2);
    assert_eq!(parsed["elapsed_seconds"], 1);
}

#[test]
fn without_wait_the_output_has_no_wait_fields() {
    let server = serve(|_| (200, packument(&["1.0.0", "1.1.0"])));
    let output = run(&server, &["--version", "1.1.0", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let object = parsed.as_object().unwrap();
    for key in ["waited", "attempts", "elapsed_seconds", "timed_out"] {
        assert!(!object.contains_key(key), "{key}");
    }
}

#[test]
fn json_keeps_progress_off_stdout() {
    let server = serve(|index| {
        if index < 2 {
            (200, packument(&["1.0.0"]))
        } else {
            (200, packument(&["1.0.0", "2.0.0"]))
        }
    });
    let output = run(
        &server,
        &[
            "--version",
            "2.0.0",
            "--wait",
            "--json",
            "--no-progress",
            "--interval",
            "1",
            "--timeout",
            "30",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let parsed: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed["attempts"], 2);
    assert_eq!(parsed["timed_out"], false);
}
