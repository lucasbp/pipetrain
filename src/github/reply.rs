use std::str::FromStr;

use serde_json::Value;
use ureq::http::HeaderMap;

use super::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset: Option<i64>,
    pub resource: String,
}

fn header<T: FromStr>(headers: &HeaderMap, name: &str) -> Option<T> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

impl Rate {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            limit: header(headers, "x-ratelimit-limit"),
            remaining: header(headers, "x-ratelimit-remaining"),
            reset: header(headers, "x-ratelimit-reset"),
            resource: header(headers, "x-ratelimit-resource").unwrap_or_else(|| "core".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub rate: Rate,
    pub body: Value,
}

fn message_of(body: &Value) -> String {
    body.get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("no message")
        .to_string()
}

fn mentions_sso(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "sso" || word.starts_with("saml"))
}

fn mentions_rate_limit(text: &str) -> bool {
    text.to_lowercase().contains("rate limit")
}

impl Reply {
    pub fn into_body(self, label: &str) -> Result<Value, ApiError> {
        match self.failure(label) {
            Some(error) => Err(error),
            None => Ok(self.body),
        }
    }

    pub fn into_graphql(self, label: &str) -> Result<Value, ApiError> {
        let reset = self.rate.reset;
        let body = self.into_body(label)?;
        match graphql_failure(&body, label, reset) {
            Some(error) => Err(error),
            None => Ok(body),
        }
    }

    fn failure(&self, label: &str) -> Option<ApiError> {
        let message = message_of(&self.body);
        Some(match self.status {
            0..=299 => return None,
            300..=399 => ApiError::Other {
                message: format!(
                    "GitHub redirected {label}: the repo was probably renamed or moved; update the reference"
                ),
            },
            401 => ApiError::Unauthorized,
            403 | 429 => self.refused(message),
            404 => ApiError::NotFound {
                what: label.to_string(),
            },
            500.. => ApiError::Other {
                message: format!(
                    "GitHub answered {} for {label}: {message}. This is usually temporary on GitHub's side: try again in a minute.",
                    self.status
                ),
            },
            status => ApiError::Other {
                message: format!(
                    "GitHub answered {status} for {label}: {message}. Run with --verbose to see which request failed."
                ),
            },
        })
    }

    fn refused(&self, message: String) -> ApiError {
        let limited = ApiError::RateLimited {
            reset: self.rate.reset,
        };
        if self.status == 429 || self.rate.remaining == Some(0) {
            limited
        } else if mentions_sso(&message) {
            ApiError::Sso { message }
        } else if mentions_rate_limit(&message) {
            limited
        } else {
            ApiError::Forbidden { message }
        }
    }
}

fn graphql_failure(body: &Value, label: &str, reset: Option<i64>) -> Option<ApiError> {
    let errors = body.get("errors")?.as_array()?;
    let kind = |e: &Value| e.get("type").and_then(|t| t.as_str()).map(str::to_string);
    let only_not_found = errors
        .iter()
        .all(|e| kind(e).as_deref() == Some("NOT_FOUND"));
    if only_not_found && body.get("data").is_some_and(|d| !d.is_null()) {
        return None;
    }
    let message = errors
        .iter()
        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
        .collect::<Vec<_>>()
        .join("; ");
    if errors
        .iter()
        .any(|e| kind(e).as_deref() == Some("RATE_LIMITED"))
    {
        return Some(ApiError::RateLimited { reset });
    }
    if mentions_sso(&message) {
        return Some(ApiError::Sso { message });
    }
    Some(ApiError::Graphql {
        label: label.to_string(),
        message,
    })
}
