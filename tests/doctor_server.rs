use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

use pipetrain::config::ApiConfig;
use pipetrain::doctor::check::{Check, Status};
use pipetrain::doctor::{remote, report};
use pipetrain::github::{ApiError, Client};
use pipetrain::hint;
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

struct Canned {
    status: &'static str,
    body: String,
}

fn canned(status: &'static str, body: Value) -> Canned {
    Canned {
        status,
        body: body.to_string(),
    }
}

fn serve(replies: Vec<Canned>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            seen.push(read_request(&mut stream));
            stream.write_all(render(&reply).as_bytes()).unwrap();
        }
        seen
    });
    (base, handle)
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

fn render(reply: &Canned) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    )
}

fn client(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

fn orgs(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

fn viewer() -> Canned {
    canned(
        "200 OK",
        json!({ "data": { "viewer": { "login": "octocat" } } }),
    )
}

fn rate(remaining: u64) -> Canned {
    canned(
        "200 OK",
        json!({ "resources": { "core": { "remaining": remaining, "limit": 5000, "reset": 4_000_000_000_i64 } } }),
    )
}

fn org_found(private: u64) -> Canned {
    canned(
        "200 OK",
        json!({ "data": { "organization": { "login": "acme", "repositories": { "totalCount": private } } } }),
    )
}

fn org_missing() -> Canned {
    canned(
        "200 OK",
        json!({
            "data": { "organization": null },
            "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to an Organization" }]
        }),
    )
}

fn user_missing() -> Canned {
    canned(
        "200 OK",
        json!({
            "data": { "user": null },
            "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a User" }]
        }),
    )
}

fn user_found() -> Canned {
    canned(
        "200 OK",
        json!({ "data": { "user": { "login": "octocat" } } }),
    )
}

fn org_sso() -> Canned {
    canned(
        "200 OK",
        json!({
            "data": { "organization": null },
            "errors": [{
                "type": "FORBIDDEN",
                "message": "Resource protected by organization SAML enforcement. You must grant your OAuth token access to this organization."
            }]
        }),
    )
}

fn run(replies: Vec<Canned>, names: &[&str]) -> (Vec<Check>, Vec<String>) {
    let (base, server) = serve(replies);
    let checks = remote(client(&base), &orgs(names));
    (checks, server.join().unwrap())
}

fn statuses(checks: &[Check]) -> Vec<(&str, Status)> {
    checks.iter().map(|c| (c.id.as_str(), c.status)).collect()
}

#[test]
fn happy_path_passes_every_check() {
    let (checks, seen) = run(vec![viewer(), rate(4_900), org_found(7)], &["acme"]);
    assert_eq!(
        statuses(&checks),
        [
            ("api", Status::Ok),
            ("rate", Status::Ok),
            ("org:acme", Status::Ok)
        ]
    );
    assert_eq!(report::exit_code(&checks), 0);
    assert!(seen[0].starts_with("POST /graphql"));
    assert!(seen[1].starts_with("GET /rate_limit"));
    assert!(seen[2].starts_with("POST /graphql"));
}

#[test]
fn every_request_is_a_get_or_a_graphql_query() {
    let (_, seen) = run(vec![viewer(), rate(4_900), org_found(1)], &["acme"]);
    for request in &seen {
        assert!(request.starts_with("GET ") || request.starts_with("POST /graphql"));
        let lower = request.to_lowercase();
        assert!(!lower.contains("mutation"));
    }
}

#[test]
fn the_token_is_sent_but_never_printed() {
    let (checks, seen) = run(vec![viewer(), rate(4_900), org_found(1)], &["acme"]);
    assert!(seen[0].to_lowercase().contains(&FAKE.to_lowercase()));
    assert!(!report::json(&checks).contains(FAKE));
    assert!(!report::text(&checks).contains(FAKE));
}

#[test]
fn low_rate_warns_but_does_not_fail() {
    let (checks, _) = run(vec![viewer(), rate(10), org_found(1)], &["acme"]);
    assert_eq!(checks[1].status, Status::Warn);
    assert_eq!(report::exit_code(&checks), 0);
}

#[test]
fn a_user_account_is_found_through_the_fallback_query() {
    let replies = vec![viewer(), rate(4_900), org_missing(), user_found()];
    let (checks, seen) = run(replies, &["octocat"]);
    assert_eq!(checks[2].status, Status::Ok);
    assert!(checks[2].message.contains("user account"));
    assert_eq!(seen.len(), 4);
}

#[test]
fn a_name_nobody_can_see_fails() {
    let replies = vec![viewer(), rate(4_900), org_missing(), user_missing()];
    let (checks, _) = run(replies, &["ghost"]);
    assert_eq!(checks[2].status, Status::Fail);
    assert_eq!(report::exit_code(&checks), 1);
}

#[test]
fn an_org_with_no_private_repos_warns_about_sso() {
    let (checks, _) = run(vec![viewer(), rate(4_900), org_found(0)], &["acme"]);
    assert_eq!(checks[2].status, Status::Warn);
    assert_eq!(checks[2].fix, Some(hint::sso_fix()));
}

#[test]
fn an_sso_answer_for_an_org_warns_with_the_sso_fix() {
    let (checks, _) = run(vec![viewer(), rate(4_900), org_sso()], &["acme"]);
    assert_eq!(checks[2].status, Status::Warn);
    assert_eq!(checks[2].fix, Some(hint::sso_fix()));
}

#[test]
fn each_org_gets_its_own_line() {
    let replies = vec![viewer(), rate(4_900), org_found(3), org_found(5)];
    let (checks, _) = run(replies, &["acme", "other"]);
    let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["api", "rate", "org:acme", "org:other"]);
}

#[test]
fn a_401_fails_the_api_check_and_skips_the_rest() {
    let reply = canned("401 Unauthorized", json!({ "message": "Bad credentials" }));
    let (checks, seen) = run(vec![reply], &["acme"]);
    assert_eq!(
        statuses(&checks),
        [
            ("api", Status::Fail),
            ("rate", Status::Warn),
            ("org:acme", Status::Warn)
        ]
    );
    assert_eq!(checks[0].fix, Some(hint::renew_token_fix()));
    assert!(checks[1].message.starts_with("skipped"));
    assert_eq!(seen.len(), 1);
    assert_eq!(report::exit_code(&checks), 1);
}

#[test]
fn a_403_mentioning_sso_fails_the_api_check_with_the_sso_fix() {
    let reply = canned(
        "403 Forbidden",
        json!({ "message": "Resource protected by organization SAML enforcement." }),
    );
    let (checks, _) = run(vec![reply], &[]);
    assert_eq!(checks[0].status, Status::Fail);
    assert_eq!(checks[0].fix, Some(hint::sso_fix()));
}

#[test]
fn a_plain_403_fails_the_api_check() {
    let reply = canned(
        "403 Forbidden",
        json!({ "message": "Resource not accessible" }),
    );
    let (checks, _) = run(vec![reply], &[]);
    assert_eq!(checks[0].status, Status::Fail);
    assert!(checks[0].message.contains("Resource not accessible"));
    assert_eq!(report::exit_code(&checks), 1);
}

#[test]
fn a_refused_connection_is_a_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let error = client(&base)
        .query("viewer", "query { viewer { login } }", json!({}))
        .unwrap_err();
    assert!(matches!(error, ApiError::Network));
}

#[test]
fn a_refused_connection_fails_the_api_check_with_the_connectivity_fix() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let checks = remote(client(&base), &orgs(&["acme"]));
    assert_eq!(checks[0].status, Status::Fail);
    assert_eq!(checks[0].fix, Some(hint::connectivity_fix()));
    assert_eq!(checks.len(), 3);
    assert_eq!(report::exit_code(&checks), 1);
}
