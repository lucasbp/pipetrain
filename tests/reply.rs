use pipetrain::github::{ApiError, Rate, Reply};
use serde_json::{Value, json};

fn rate(remaining: Option<u64>, reset: Option<i64>) -> Rate {
    Rate {
        limit: Some(5000),
        remaining,
        reset,
        resource: "core".to_string(),
    }
}

fn reply(status: u16, remaining: Option<u64>, body: Value) -> Reply {
    Reply {
        status,
        rate: rate(remaining, Some(1_700_000_000)),
        body,
    }
}

fn failure(status: u16, remaining: Option<u64>, body: Value) -> ApiError {
    reply(status, remaining, body)
        .into_body("acme/api#7")
        .unwrap_err()
}

#[test]
fn success_returns_the_body() {
    let body = reply(200, Some(10), json!({"ok": true}))
        .into_body("x")
        .unwrap();
    assert_eq!(body, json!({"ok": true}));
}

#[test]
fn redirect_asks_for_an_updated_reference() {
    let error = failure(301, None, Value::Null);
    assert!(error.to_string().contains("renamed or moved"), "{error}");
}

#[test]
fn unauthorized_is_401() {
    assert_eq!(failure(401, Some(10), Value::Null), ApiError::Unauthorized);
}

#[test]
fn forbidden_with_nothing_left_is_a_rate_limit() {
    let error = failure(403, Some(0), json!({"message": "API rate limit exceeded"}));
    assert_eq!(
        error,
        ApiError::RateLimited {
            reset: Some(1_700_000_000)
        }
    );
}

#[test]
fn status_429_is_a_rate_limit() {
    let error = failure(429, Some(40), Value::Null);
    assert!(matches!(error, ApiError::RateLimited { .. }));
}

#[test]
fn secondary_limit_is_found_by_message() {
    let body = json!({"message": "You have exceeded a secondary rate limit"});
    assert!(matches!(
        failure(403, Some(40), body),
        ApiError::RateLimited { .. }
    ));
}

#[test]
fn saml_message_is_sso() {
    let body = json!({"message": "Resource protected by organization SAML enforcement"});
    assert!(matches!(failure(403, Some(40), body), ApiError::Sso { .. }));
}

#[test]
fn similar_words_are_not_sso() {
    let body = json!({"message": "possession of this token is not enough"});
    assert!(matches!(
        failure(403, Some(40), body),
        ApiError::Forbidden { .. }
    ));
}

#[test]
fn plain_forbidden_keeps_the_message() {
    let error = failure(403, Some(40), json!({"message": "Resource not accessible"}));
    assert_eq!(
        error,
        ApiError::Forbidden {
            message: "Resource not accessible".into()
        }
    );
}

#[test]
fn not_found_carries_the_label() {
    assert_eq!(
        failure(404, Some(40), Value::Null),
        ApiError::NotFound {
            what: "acme/api#7".into()
        }
    );
}

#[test]
fn server_errors_are_called_temporary() {
    let error = failure(502, Some(40), json!({"message": "Bad Gateway"}));
    assert!(error.to_string().contains("temporary"), "{error}");
}

#[test]
fn unexpected_status_points_at_verbose() {
    let error = failure(418, Some(40), Value::Null);
    assert!(error.to_string().contains("--verbose"), "{error}");
}

fn graphql(body: Value) -> Result<Value, ApiError> {
    reply(200, Some(4000), body).into_graphql("search")
}

#[test]
fn graphql_not_found_with_data_is_fine() {
    let body = json!({"data": {"repository": null, "viewer": {}}, "errors": [{"type": "NOT_FOUND", "message": "gone"}]});
    assert!(graphql(body).is_ok());
}

#[test]
fn graphql_not_found_without_data_fails() {
    let body = json!({"data": null, "errors": [{"type": "NOT_FOUND", "message": "gone"}]});
    assert!(matches!(graphql(body), Err(ApiError::Graphql { .. })));
}

#[test]
fn graphql_rate_limit_type_is_recognised() {
    let body = json!({"errors": [{"type": "RATE_LIMITED", "message": "slow down"}]});
    assert_eq!(
        graphql(body).unwrap_err(),
        ApiError::RateLimited {
            reset: Some(1_700_000_000)
        }
    );
}

#[test]
fn graphql_errors_join_their_messages() {
    let body = json!({"errors": [{"message": "one"}, {"message": "two"}]});
    let Err(ApiError::Graphql { label, message }) = graphql(body) else {
        panic!("expected a graphql error");
    };
    assert_eq!((label.as_str(), message.as_str()), ("search", "one; two"));
}

#[test]
fn graphql_saml_message_is_sso() {
    let body = json!({"errors": [{"message": "SAML enforcement blocks this token"}]});
    assert!(matches!(graphql(body), Err(ApiError::Sso { .. })));
}
