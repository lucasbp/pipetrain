use std::fmt;
use std::process::Command;

use anyhow::Result;

use super::error::ApiError;
use crate::hint::GhState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Gh,
    Env,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Lookup {
    value: String,
    source: Source,
}

impl Lookup {
    pub fn source(&self) -> Source {
        self.source
    }

    pub fn into_token(self) -> String {
        self.value
    }
}

impl fmt::Debug for Lookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lookup")
            .field("source", &self.source)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum GhProbe {
    Missing,
    LoggedOut,
    Token(String),
}

impl GhProbe {
    pub fn state(&self) -> GhState {
        match self {
            Self::Missing => GhState::Missing,
            Self::LoggedOut => GhState::LoggedOut,
            Self::Token(_) => GhState::LoggedIn,
        }
    }
}

impl fmt::Debug for GhProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("Missing"),
            Self::LoggedOut => f.write_str("LoggedOut"),
            Self::Token(_) => f.write_str("Token(<redacted>)"),
        }
    }
}

pub fn probe_gh() -> GhProbe {
    let Ok(out) = Command::new("gh").args(["auth", "token"]).output() else {
        return GhProbe::Missing;
    };
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !value.is_empty() {
        GhProbe::Token(value)
    } else {
        GhProbe::LoggedOut
    }
}

pub fn resolve(gh: GhProbe, env: Option<String>) -> Result<Lookup, ApiError> {
    let env = env
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match (gh, env) {
        (GhProbe::Token(value), Some(env)) if value == env => Ok(from_env(env)),
        (GhProbe::Token(value), _) => Ok(Lookup {
            value,
            source: Source::Gh,
        }),
        (_, Some(env)) => Ok(from_env(env)),
        (_, None) => Err(ApiError::NoToken),
    }
}

fn from_env(value: String) -> Lookup {
    Lookup {
        value,
        source: Source::Env,
    }
}

pub fn lookup() -> Result<Lookup, ApiError> {
    resolve(probe_gh(), std::env::var("GH_TOKEN").ok())
}

pub fn token() -> Result<String> {
    Ok(lookup()?.into_token())
}
