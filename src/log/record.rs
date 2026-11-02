use serde::{Deserialize, Serialize};

use crate::github::ApiError;
use crate::update::error::UpdateError;

use super::stats::Counts;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub time: String,
    pub version: String,
    pub command: String,
    pub exit: i32,
    pub error: Option<String>,
    pub variant: Option<String>,
    pub duration_ms: u64,
    pub requests: u32,
    pub cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorKind {
    pub kind: &'static str,
    pub variant: Option<&'static str>,
}

pub fn kind_of(error: &anyhow::Error) -> ErrorKind {
    if let Some(api) = error.downcast_ref::<ApiError>() {
        return ErrorKind {
            kind: "ApiError",
            variant: Some(api.variant()),
        };
    }
    if let Some(update) = error.downcast_ref::<UpdateError>() {
        return ErrorKind {
            kind: "UpdateError",
            variant: Some(update.variant()),
        };
    }
    ErrorKind {
        kind: "Other",
        variant: None,
    }
}

pub fn build(
    command: &str,
    exit: i32,
    error: Option<ErrorKind>,
    duration_ms: u64,
    counts: Counts,
) -> Record {
    Record {
        time: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        version: env!("CARGO_PKG_VERSION").to_string(),
        command: command.to_string(),
        exit,
        error: error.as_ref().map(|found| found.kind.to_string()),
        variant: error
            .and_then(|found| found.variant)
            .map(ToString::to_string),
        duration_ms,
        requests: counts.requests,
        cache_hit: counts.cache_hits > 0,
    }
}
