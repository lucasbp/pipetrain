use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::github::{ApiError, Lookup, Source, describe_reset};
use crate::hint::{self, GhState, Os};

pub const LOW_RATE: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: String,
    pub status: Status,
    pub message: String,
    pub fix: Option<String>,
}

impl Check {
    fn new(id: &str, status: Status, message: impl Into<String>, fix: Option<String>) -> Self {
        Self {
            id: id.to_string(),
            status,
            message: message.into(),
            fix,
        }
    }

    pub fn ok(id: &str, message: impl Into<String>) -> Self {
        Self::new(id, Status::Ok, message, None)
    }

    pub fn warn(id: &str, message: impl Into<String>, fix: Option<String>) -> Self {
        Self::new(id, Status::Warn, message, fix)
    }

    pub fn fail(id: &str, message: impl Into<String>, fix: Option<String>) -> Self {
        Self::new(id, Status::Fail, message, fix)
    }

    pub fn skipped(id: &str, reason: &str) -> Self {
        Self::new(id, Status::Warn, format!("skipped: {reason}"), None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigState {
    Found(PathBuf),
    NotFound,
    Invalid(String),
}

pub fn token(found: &Result<Lookup, ApiError>, gh: GhState, os: Os) -> Check {
    match found {
        Ok(lookup) => Check::ok("token", source_message(lookup.source())),
        Err(_) => Check::fail(
            "token",
            hint::short_no_token(os, gh),
            Some(hint::missing_token_fix(os, gh)),
        ),
    }
}

fn source_message(source: Source) -> &'static str {
    match source {
        Source::Gh => "found a token from `gh auth token`",
        Source::Env => "found a token in GH_TOKEN",
    }
}

pub fn failure(id: &str, error: &ApiError) -> Check {
    match error {
        ApiError::Unauthorized => Check::fail(
            id,
            "GitHub rejected the token (401): it is invalid or expired",
            Some(hint::renew_token_fix()),
        ),
        ApiError::Network => Check::fail(
            id,
            "cannot reach api.github.com",
            Some(hint::connectivity_fix()),
        ),
        ApiError::Sso { .. } => Check::fail(id, error.to_string(), Some(hint::sso_fix())),
        ApiError::RateLimited { .. } => Check::warn(id, error.to_string(), None),
        other => Check::fail(id, other.to_string(), None),
    }
}

pub fn api(result: &Result<Value, ApiError>) -> Check {
    match result {
        Err(error) => failure("api", error),
        Ok(body) => match body.pointer("/data/viewer/login").and_then(Value::as_str) {
            Some(login) => Check::ok("api", format!("GraphQL answered, signed in as {login}")),
            None => Check::fail("api", "GraphQL answered without a viewer login", None),
        },
    }
}

pub fn rate(result: &Result<Value, ApiError>, now: i64) -> Check {
    let body = match result {
        Err(error) => return failure("rate", error),
        Ok(body) => body,
    };
    let core = body.pointer("/resources/core");
    let Some(remaining) = core
        .and_then(|c| c.get("remaining"))
        .and_then(Value::as_u64)
    else {
        return Check::fail("rate", "the rate limit answer had no core numbers", None);
    };
    let limit = core.and_then(|c| c.get("limit")).and_then(Value::as_u64);
    let reset = core.and_then(|c| c.get("reset")).and_then(Value::as_i64);
    let of = limit.map(|l| format!(" of {l}")).unwrap_or_default();
    let message = format!(
        "core: {remaining}{of} requests left; {}",
        describe_reset(now, reset)
    );
    if remaining < LOW_RATE {
        let fix = "Wait for the reset before running `plan`, or use a different token.";
        return Check::warn("rate", message, Some(fix.to_string()));
    }
    Check::ok("rate", message)
}

pub fn config(state: &ConfigState) -> Check {
    match state {
        ConfigState::Found(path) => Check::ok("config", format!("loaded {}", path.display())),
        ConfigState::NotFound => Check::warn(
            "config",
            "no config file found; `plan` needs `orgs` from a config or --org",
            Some(
                "Create ./pipetrain.toml with `orgs = [\"your-org\"]`, or pass --org.".to_string(),
            ),
        ),
        ConfigState::Invalid(message) => Check::fail("config", message.clone(), None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileState {
    Unknown,
    Chosen { name: String, label: String },
    Unresolved(String),
    Rejected(String),
}

pub fn profile(state: &ProfileState) -> Check {
    match state {
        ProfileState::Unknown => Check::skipped("profile", "no usable config"),
        ProfileState::Chosen { name, label } => Check::ok("profile", format!("{name} ({label})")),
        ProfileState::Unresolved(message) => Check::warn(
            "profile",
            message.clone(),
            Some("Pass --profile NAME or run from a profile folder.".to_string()),
        ),
        ProfileState::Rejected(message) => Check::fail(
            "profile",
            message.clone(),
            Some("Pass one of the listed names to --profile.".to_string()),
        ),
    }
}
