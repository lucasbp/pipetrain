use std::fmt;

use crate::hint::{self, GhState, Os};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    NoToken,
    Unauthorized,
    RateLimited { reset: Option<i64> },
    Sso { message: String },
    Forbidden { message: String },
    NotFound { what: String },
    Network,
    Graphql { label: String, message: String },
    Stopped { message: String },
    Other { message: String },
}

fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("{count} {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

pub fn describe_reset(now: i64, reset: Option<i64>) -> String {
    let Some(at) = reset else {
        return "GitHub did not say when it resets".to_string();
    };
    let left = at - now;
    if left <= 0 {
        "it should already have reset".to_string()
    } else if left < 60 {
        format!("it resets in {}", plural(left, "second"))
    } else {
        format!("it resets in about {}", plural((left + 59) / 60, "minute"))
    }
}

fn rate_limited(reset: Option<i64>) -> String {
    let when = describe_reset(chrono::Utc::now().timestamp(), reset);
    format!("GitHub rate limit reached: {when}. Run the command again once it resets.")
}

fn sso(message: &str) -> String {
    format!(
        "GitHub blocked the request: \"{message}\". The org likely enforces SAML SSO: authorize the token for the org in your GitHub token settings, then run the command again."
    )
}

fn forbidden(message: &str) -> String {
    format!(
        "GitHub refused the request (403): \"{message}\". The token may lack read access here: it needs read-only Pull requests, Contents and Metadata on the repos or org."
    )
}

fn not_found(what: &str) -> String {
    format!(
        "not found: {what} (404). Check the name; the token may not see private repos, so also check that it has access to the org."
    )
}

fn graphql(label: &str, message: &str) -> String {
    format!(
        "GitHub GraphQL error in {label}: {message}. This is often temporary or a missing permission on the token: try again, and run with --verbose to see the requests."
    )
}

impl ApiError {
    fn text(&self) -> String {
        match self {
            Self::NoToken => hint::short_no_token(Os::current(), GhState::LoggedOut),
            Self::Unauthorized => format!(
                "GitHub rejected the token (401): it is invalid or expired. {}",
                hint::renew_token_fix()
            ),
            Self::RateLimited { reset } => rate_limited(*reset),
            Self::Sso { message } => sso(message),
            Self::Forbidden { message } => forbidden(message),
            Self::NotFound { what } => not_found(what),
            Self::Network => format!("cannot reach api.github.com. {}", hint::connectivity_fix()),
            Self::Graphql { label, message } => graphql(label, message),
            Self::Stopped { message } | Self::Other { message } => message.clone(),
        }
    }

    pub fn redacted(self, secret: &str) -> Self {
        if secret.is_empty() {
            return self;
        }
        let scrub = |text: String| text.replace(secret, "[redacted]");
        match self {
            Self::Sso { message } => Self::Sso {
                message: scrub(message),
            },
            Self::Forbidden { message } => Self::Forbidden {
                message: scrub(message),
            },
            Self::NotFound { what } => Self::NotFound { what: scrub(what) },
            Self::Graphql { label, message } => Self::Graphql {
                label: scrub(label),
                message: scrub(message),
            },
            Self::Stopped { message } => Self::Stopped {
                message: scrub(message),
            },
            Self::Other { message } => Self::Other {
                message: scrub(message),
            },
            other => other,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl std::error::Error for ApiError {}
