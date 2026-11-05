use std::time::Duration;

mod common;

use common::{Canned, canned, serve};
use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client, Retry};
use serde_json::json;

const FAKE: &str = "ghp_FAKE0123456789";
const PATH: &str = "/repos/acme/api/pulls/7";
const SECONDARY: &str = r#"{"message":"You have exceeded a secondary rate limit."}"#;

fn policy() -> Retry {
    Retry {
        max_retries: 3,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(5),
    }
}

fn plain(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

fn client(base: &str) -> Client {
    plain(base).with_retry(policy())
}

fn ok() -> Canned {
    canned("200 OK", r#"{"number":7}"#)
}

fn with_remaining(mut reply: Canned, remaining: &str) -> Canned {
    reply.headers = vec![("x-ratelimit-remaining", remaining.to_string())];
    reply
}

#[test]
fn a_502_is_retried_and_the_next_answer_wins() {
    let (base, server) = serve(vec![canned("502 Bad Gateway", "{}"), ok()]);
    let mut client = client(&base);
    let reply = client.get(PATH).unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, json!({"number": 7}));
    assert_eq!(client.requests(), 2);
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn a_graphql_query_is_retried_through_two_502s() {
    let data = r#"{"data":{"viewer":{"login":"octo"}}}"#;
    let (base, server) = serve(vec![
        canned("502 Bad Gateway", "{}"),
        canned("502 Bad Gateway", "{}"),
        canned("200 OK", data),
    ]);
    let mut client = client(&base);
    let body = client
        .graphql("viewer", "query { viewer { login } }", json!({}))
        .unwrap();
    assert_eq!(body.pointer("/data/viewer/login"), Some(&json!("octo")));
    assert_eq!(client.requests(), 3);
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn a_secondary_rate_limit_403_is_retried() {
    let limited = with_remaining(canned("403 Forbidden", SECONDARY), "4000");
    let (base, server) = serve(vec![limited, ok()]);
    let mut client = client(&base);
    let reply = client.get(PATH).unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(client.requests(), 2);
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn a_403_with_an_exhausted_quota_is_reported_not_retried() {
    let exhausted = with_remaining(canned("403 Forbidden", SECONDARY), "0");
    let (base, server) = serve(vec![exhausted]);
    let mut client = client(&base);
    let error = client
        .rest(PATH)
        .unwrap_err()
        .downcast::<ApiError>()
        .unwrap();
    assert!(matches!(error, ApiError::RateLimited { .. }), "{error:?}");
    assert_eq!(client.requests(), 1);
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn a_404_is_not_retried() {
    let (base, server) = serve(vec![canned("404 Not Found", r#"{"message":"Not Found"}"#)]);
    let mut client = client(&base);
    let reply = client.get(PATH).unwrap();
    assert_eq!(reply.status, 404);
    assert_eq!(client.requests(), 1);
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn a_plain_403_is_not_retried() {
    let denied = with_remaining(
        canned("403 Forbidden", r#"{"message":"Resource not accessible"}"#),
        "4000",
    );
    let (base, server) = serve(vec![denied]);
    let mut client = client(&base);
    let reply = client.get(PATH).unwrap();
    assert_eq!(reply.status, 403);
    assert_eq!(client.requests(), 1);
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn a_server_that_keeps_failing_stops_after_the_last_retry_and_hides_the_token() {
    let replies = (0..4)
        .map(|_| canned("503 Service Unavailable", "{}"))
        .collect();
    let (base, server) = serve(replies);
    let mut client = client(&base);
    let error = client
        .rest(PATH)
        .unwrap_err()
        .downcast::<ApiError>()
        .unwrap();
    let ApiError::Other { message } = &error else {
        panic!("expected Other, got {error:?}");
    };
    assert!(message.contains("503"), "{message}");
    assert!(!message.contains(FAKE), "{message}");
    assert!(!format!("{error:?}").contains(FAKE));
    assert_eq!(client.requests(), 4);
    assert_eq!(server.join().unwrap().len(), 4);
}

#[test]
fn without_a_retry_policy_a_502_fails_on_the_first_request() {
    let (base, server) = serve(vec![canned("502 Bad Gateway", "{}")]);
    let mut client = plain(&base);
    let error = client
        .rest(PATH)
        .unwrap_err()
        .downcast::<ApiError>()
        .unwrap();
    assert!(matches!(error, ApiError::Other { .. }), "{error:?}");
    assert_eq!(client.requests(), 1);
    assert_eq!(server.join().unwrap().len(), 1);
}
