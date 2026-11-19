use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

mod common;

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn serve(status: u16, body: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);
            log.lock().unwrap().push(request);
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
    let mut text = String::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap_or(0);
        text.push_str(&String::from_utf8_lossy(&chunk[..read]));
        if read == 0 || text.contains("\r\n\r\n") {
            return text;
        }
    }
}

fn bump(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .arg("bump")
        .args(args)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn apply_conflicts_with_check_and_yes_needs_apply() {
    let both = bump(&["Acme.Util", "--apply", "--check"]);
    assert!(!both.status.success());
    assert!(
        stderr(&both).contains("cannot be used with"),
        "{}",
        stderr(&both)
    );
    let alone = bump(&["Acme.Util", "--yes"]);
    assert!(!alone.status.success());
    assert!(stderr(&alone).contains("--apply"), "{}", stderr(&alone));
}

#[test]
fn a_refused_feed_keeps_the_feed_message_and_exit_code() {
    let server = serve(401, "{}");
    let output = bump(&[
        "@acme/ui",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--repo",
        "acme/web",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("read:packages"),
        "{}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty());
}

#[test]
fn a_package_missing_from_the_feed_exits_1() {
    let server = serve(404, "{}");
    let output = bump(&[
        "@acme/ui",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--repo",
        "acme/web",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(&format!("package not found in {}", server.base)),
        "{}",
        stderr(&output)
    );
    assert_eq!(server.requests(), 1);
}

#[test]
fn without_repos_or_profile_names_it_stops_before_the_feed() {
    let server = serve(500, "{}");
    let scratch = common::TempDir::new("bump");
    let path = scratch.join("pipetrain.toml");
    std::fs::write(&path, "orgs = [\"acme\"]\n").unwrap();
    let output = bump(&[
        "@acme/ui",
        "--to",
        "2.0.0",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--config",
        path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no repos given"),
        "{}",
        stderr(&output)
    );
    assert_eq!(server.requests(), 0);
}

#[test]
fn an_unknown_level_lists_the_valid_ones() {
    let output = bump(&["Acme.Util", "--only", "huge"]);
    assert_eq!(output.status.code(), Some(2));
    let text = stderr(&output);
    assert!(text.contains("invalid value 'huge'"), "{text}");
    for level in ["major", "minor", "patch"] {
        assert!(text.contains(level), "{text}");
    }
}

fn passes_the_parser(extra: &[&str]) {
    let server = serve(401, "{}");
    let mut args = vec![
        "@acme/ui",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--repo",
        "acme/web",
    ];
    args.extend_from_slice(extra);
    let output = bump(&args);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("read:packages"),
        "{}",
        stderr(&output)
    );
    assert_eq!(server.requests(), 1);
}

#[test]
fn only_can_be_repeated() {
    passes_the_parser(&["--only", "major", "--only", "minor"]);
}

#[test]
fn changes_only_and_check_are_accepted_together() {
    passes_the_parser(&["--changes-only", "--check"]);
}

#[test]
fn the_new_flags_are_accepted_by_the_parser() {
    passes_the_parser(&[
        "--restore-mode",
        "unlocked",
        "--solution",
        "src/Acme.sln",
        "--prerelease",
    ]);
}

#[test]
fn an_unknown_restore_mode_lists_the_valid_ones() {
    let output = bump(&["Acme.Util", "--restore-mode", "fast"]);
    assert_eq!(output.status.code(), Some(2));
    let text = stderr(&output);
    assert!(text.contains("invalid value 'fast'"), "{text}");
    for mode in ["locked", "unlocked", "skip"] {
        assert!(text.contains(mode), "{text}");
    }
}

const ONLY_PRERELEASES: &str = r#"{"versions":{"2.0.0-beta.1":{},"2.0.0-beta.2":{}}}"#;

fn only_prereleases(extra: &[&str]) -> (Output, Server) {
    let server = serve(200, ONLY_PRERELEASES);
    let mut args = vec![
        "@acme/ui",
        "--ecosystem",
        "npm",
        "--registry",
        &server.base,
        "--repo",
        "acme/web",
    ];
    args.extend_from_slice(extra);
    let output = bump(&args);
    (output, server)
}

#[test]
fn without_a_stable_version_the_default_target_stops_and_names_the_way_out() {
    let (output, _server) = only_prereleases(&[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("no stable version"), "{text}");
    assert!(text.contains("--prerelease"), "{text}");
    assert!(stdout(&output).is_empty());
}

#[test]
fn prerelease_moves_past_the_stable_gate() {
    let (output, _server) = only_prereleases(&["--prerelease"]);
    assert!(
        !stderr(&output).contains("no stable version"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn json_and_color_cannot_be_combined() {
    let output = bump(&["Acme.Util", "--json", "--color", "always"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("cannot be used with"),
        "{}",
        stderr(&output)
    );
}
