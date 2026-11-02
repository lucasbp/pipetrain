use std::net::TcpListener;

mod common;

use common::{Canned, canned, serve};
use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client};
use serde_json::json;

const FAKE: &str = "ghp_FAKE0123456789";

fn client(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

fn fetch(reply: Canned) -> (Result<pipetrain::github::Reply, ApiError>, String) {
    let (base, server) = serve(vec![reply]);
    let result = client(&base).get("/repos/acme/api/pulls/7");
    (result, server.join().unwrap().remove(0))
}

fn failure(reply: Canned) -> ApiError {
    let (base, server) = serve(vec![reply]);
    let error = client(&base).rest("/repos/acme/api/pulls/7").unwrap_err();
    server.join().unwrap();
    error.downcast::<ApiError>().unwrap()
}

#[test]
fn ok_reply_carries_body_and_rate_headers() {
    let mut reply = canned("200 OK", r#"{"number":7}"#);
    reply.headers = vec![
        ("x-ratelimit-remaining", "4321".to_string()),
        ("x-ratelimit-limit", "5000".to_string()),
        ("x-ratelimit-resource", "core".to_string()),
    ];
    let (result, request) = fetch(reply);
    let reply = result.unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, json!({"number": 7}));
    assert_eq!(reply.rate.remaining, Some(4321));
    assert_eq!(reply.rate.limit, Some(5000));
    assert!(request.starts_with("GET /repos/acme/api/pulls/7 HTTP/1.1"));
}

#[test]
fn request_sends_the_expected_headers() {
    let (_, request) = fetch(canned("200 OK", "{}"));
    let lower = request.to_lowercase();
    assert!(lower.contains(&format!("authorization: bearer {}", FAKE.to_lowercase())));
    assert!(lower.contains("accept: application/vnd.github+json"));
    assert!(lower.contains("x-github-api-version: 2022-11-28"));
    assert!(lower.contains("user-agent: pipetrain/"));
}

#[test]
fn unauthorized_reply_is_unauthorized() {
    let error = failure(canned(
        "401 Unauthorized",
        r#"{"message":"Bad credentials"}"#,
    ));
    assert_eq!(error, ApiError::Unauthorized);
}

#[test]
fn exhausted_quota_is_rate_limited_with_the_reset() {
    let mut reply = canned("403 Forbidden", r#"{"message":"API rate limit exceeded"}"#);
    reply.headers = vec![
        ("x-ratelimit-remaining", "0".to_string()),
        ("x-ratelimit-reset", "1700000000".to_string()),
    ];
    let error = failure(reply);
    assert_eq!(
        error,
        ApiError::RateLimited {
            reset: Some(1_700_000_000)
        }
    );
}

#[test]
fn saml_refusal_is_sso() {
    let body = r#"{"message":"Resource protected by organization SAML enforcement"}"#;
    let error = failure(canned("403 Forbidden", body));
    assert!(matches!(error, ApiError::Sso { .. }), "{error:?}");
}

#[test]
fn plain_refusal_is_forbidden() {
    let error = failure(canned(
        "403 Forbidden",
        r#"{"message":"Resource not accessible"}"#,
    ));
    assert!(matches!(error, ApiError::Forbidden { .. }), "{error:?}");
}

#[test]
fn missing_resource_is_not_found_with_the_path() {
    let error = failure(canned("404 Not Found", r#"{"message":"Not Found"}"#));
    assert_eq!(
        error,
        ApiError::NotFound {
            what: "/repos/acme/api/pulls/7".into()
        }
    );
}

#[test]
fn server_error_is_other() {
    let error = failure(canned("503 Service Unavailable", "{}"));
    assert!(matches!(error, ApiError::Other { .. }), "{error:?}");
}

#[test]
fn echoed_token_never_reaches_the_error() {
    let body = json!({"message": format!("Resource not accessible by {FAKE}")}).to_string();
    let error = failure(canned("403 Forbidden", &body));
    assert!(!format!("{error}").contains(FAKE));
    assert!(!format!("{error:?}").contains(FAKE));
}

#[test]
fn refused_connection_is_a_network_error() {
    for _ in 0..20 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        if let Err(error) = client(&base).get("/x") {
            assert_eq!(error, ApiError::Network);
            return;
        }
    }
    panic!("every freed port was taken by another test before the request");
}

#[test]
fn graphql_posts_only_a_query() {
    let reply = canned("200 OK", r#"{"data":{"viewer":{"login":"a"}}}"#);
    let (base, server) = serve(vec![reply]);
    let data = client(&base)
        .graphql("viewer", "query { viewer { login } }", json!({}))
        .unwrap();
    let request = server.join().unwrap().remove(0);
    assert_eq!(data["data"]["viewer"]["login"], "a");
    assert!(request.starts_with("POST /graphql HTTP/1.1"), "{request}");
    assert!(request.contains("\"query\""));
    assert!(!request.contains("mutation"));
}

#[test]
fn redirects_are_not_followed() {
    let mut reply = canned("301 Moved Permanently", "{}");
    reply.headers = vec![("location", "http://127.0.0.1:9/elsewhere".to_string())];
    let (result, _) = fetch(reply);
    let outcome = result.map(|reply| reply.status);
    assert!(
        matches!(outcome, Ok(301) | Err(ApiError::Other { .. })),
        "{outcome:?}"
    );
}

#[test]
fn counter_counts_every_request() {
    let (base, server) = serve(vec![canned("200 OK", "{}"), canned("200 OK", "{}")]);
    let mut client = client(&base);
    client.get("/a").unwrap();
    client.get("/b").unwrap();
    server.join().unwrap();
    assert_eq!(client.requests(), 2);
}
