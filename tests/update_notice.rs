use std::io::Write;
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

mod common;

use common::read_head as read_request;
use pipetrain::update::notice::{fetch_latest, read};

const TOKEN: &str = "ghp_FAKEnotice0123456789";

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

fn serve(status: u16, body: &'static str, delay: Duration) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read_request(&mut stream);
            record.lock().unwrap().push(request);
            thread::sleep(delay);
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Server { base, seen }
}

const RELEASE: &str = r#"{"tag_name":"v99.0.0","assets":[]}"#;

#[test]
fn the_lookup_reads_the_latest_version() {
    let server = serve(200, RELEASE, Duration::ZERO);
    let version = fetch_latest(&server.base, TOKEN.to_string(), Duration::from_secs(3));
    assert_eq!(version.as_deref(), Some("99.0.0"));
    let seen = server.seen.lock().unwrap();
    assert!(seen[0].starts_with("GET /repos/lucasbp/pipetrain/releases/latest HTTP/1.1"));
    assert!(seen[0].to_lowercase().contains("authorization: bearer"));
}

#[test]
fn the_lookup_without_a_token_sends_no_authorization() {
    let server = serve(200, RELEASE, Duration::ZERO);
    fetch_latest(&server.base, String::new(), Duration::from_secs(3)).unwrap();
    assert!(
        !server.seen.lock().unwrap()[0]
            .to_lowercase()
            .contains("authorization")
    );
}

#[test]
fn every_failure_is_none() {
    for (status, body) in [
        (404, r#"{"message":"Not Found"}"#),
        (500, "{}"),
        (200, r#"{"tag_name":"nightly"}"#),
        (200, "not json"),
    ] {
        let server = serve(status, body, Duration::ZERO);
        assert_eq!(
            fetch_latest(&server.base, String::new(), Duration::from_secs(3)),
            None,
            "{status} {body}"
        );
    }
}

#[test]
fn a_slow_server_is_given_up_on_quickly() {
    let server = serve(200, RELEASE, Duration::from_secs(5));
    let started = std::time::Instant::now();
    let version = fetch_latest(&server.base, String::new(), Duration::from_millis(300));
    assert_eq!(version, None);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn an_unreachable_server_is_none() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        fetch_latest(&base, String::new(), Duration::from_secs(1)),
        None
    );
}

fn cache_dir(name: &str) -> common::TempDir {
    common::TempDir::new(&format!("notice-cli-{name}"))
}

fn run(args: &[&str], dir: &std::path::Path, server: &Server, off: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
    command
        .args(args)
        .env("PIPETRAIN_CACHE_DIR", dir)
        .env("PIPETRAIN_UPDATE_API_URL", &server.base)
        .env("GH_TOKEN", TOKEN)
        .env_remove("CI");
    if off {
        command.env("PIPETRAIN_NO_UPDATE_CHECK", "1");
    }
    command.output().unwrap()
}

#[test]
fn a_redirected_run_never_looks_or_prints() {
    let scratch = cache_dir("redirected");
    let dir = scratch.join("cache");
    let server = serve(200, RELEASE, Duration::ZERO);
    let out = run(
        &["profiles", "--config", "missing.toml"],
        &dir,
        &server,
        false,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("available, run pipetrain update"),
        "{stderr}"
    );
    assert!(!stderr.contains(TOKEN));
    assert!(server.seen.lock().unwrap().is_empty());
    assert!(read(&dir.join("update-check.json")).is_none());
}

#[test]
fn the_env_var_and_the_flag_are_accepted_everywhere() {
    let scratch = cache_dir("off");
    let dir = scratch.join("cache");
    let server = serve(200, RELEASE, Duration::ZERO);
    let off = run(
        &["profiles", "--config", "missing.toml"],
        &dir,
        &server,
        true,
    );
    let flag = run(
        &["profiles", "--config", "missing.toml", "--no-update-check"],
        &dir,
        &server,
        false,
    );
    for out in [off, flag] {
        assert!(!String::from_utf8_lossy(&out.stderr).contains("available, run"));
    }
    assert!(server.seen.lock().unwrap().is_empty());
}

#[test]
fn the_flag_is_global_and_listed_in_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(["--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("--no-update-check"));
}
