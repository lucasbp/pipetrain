use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;
use ureq::Agent;

use crate::feed::{self, Ecosystem, Report};
use crate::{github, hint};

const NPM_ACCEPT: &str = "application/vnd.npm.install-v1+json";

enum Outcome {
    Body(Value),
    NotFound,
    Refused { had_token: bool },
}

pub(crate) struct Session {
    agent: Agent,
    token: OnceCell<Option<String>>,
}

impl Session {
    pub(crate) fn new() -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(40)))
            .user_agent(concat!("pipetrain/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: config.into(),
            token: OnceCell::new(),
        }
    }

    fn token(&self) -> Option<&str> {
        self.token
            .get_or_init(|| github::lookup().ok().map(github::Lookup::into_token))
            .as_deref()
    }

    fn authorization(&self, ecosystem: Ecosystem, url: &str) -> Option<String> {
        if !feed::is_github_packages(url) {
            return None;
        }
        let token = self.token()?;
        Some(match ecosystem {
            Ecosystem::Npm => feed::bearer(token),
            Ecosystem::Nuget => feed::basic(token),
        })
    }

    fn get(&self, ecosystem: Ecosystem, url: &str) -> Result<Outcome> {
        let authorization = self.authorization(ecosystem, url);
        let mut request = self.agent.get(url);
        if ecosystem == Ecosystem::Npm {
            request = request.header("Accept", NPM_ACCEPT);
        }
        if let Some(value) = &authorization {
            request = request.header("Authorization", value);
        }
        let host = feed::host(url);
        let mut response = match request.call() {
            Ok(response) => response,
            Err(
                ureq::Error::Timeout(_)
                | ureq::Error::HostNotFound
                | ureq::Error::ConnectionFailed
                | ureq::Error::Io(_),
            ) => bail!("cannot reach {host}. {}", hint::connectivity_fix()),
            Err(other) => bail!("request failed: {host}: {other}"),
        };
        match response.status().as_u16() {
            200..=299 => response
                .body_mut()
                .read_json::<Value>()
                .map(Outcome::Body)
                .map_err(|_| anyhow::anyhow!("{host} did not answer with valid JSON")),
            404 => Ok(Outcome::NotFound),
            401 | 403 => Ok(Outcome::Refused {
                had_token: authorization.is_some(),
            }),
            status => bail!("{host} answered with status {status}"),
        }
    }
}

pub(crate) enum Fetched {
    Found { feed: String, versions: Vec<String> },
    NotFound { feed: String },
    Refused { had_token: bool },
}

fn fetch_npm(
    session: &Session,
    package: &str,
    org: Option<&str>,
    registry: Option<&str>,
) -> Result<Fetched> {
    let base = feed::npm_base(package, org, registry);
    let url = feed::npm_url(&base, package);
    Ok(match session.get(Ecosystem::Npm, &url)? {
        Outcome::Body(body) => Fetched::Found {
            feed: base,
            versions: feed::npm_versions(&body),
        },
        Outcome::NotFound => Fetched::NotFound { feed: base },
        Outcome::Refused { had_token } => Fetched::Refused { had_token },
    })
}

fn fetch_nuget(
    session: &Session,
    package: &str,
    org: Option<&str>,
    registry: Option<&str>,
) -> Result<Fetched> {
    let Some(index_url) = feed::nuget_index_url(org, registry) else {
        bail!("--org is required for nuget unless --registry is given");
    };
    let index = match session.get(Ecosystem::Nuget, &index_url)? {
        Outcome::Body(body) => body,
        Outcome::NotFound => bail!(
            "the NuGet service index was not found at {}",
            feed::host(&index_url)
        ),
        Outcome::Refused { had_token } => return Ok(Fetched::Refused { had_token }),
    };
    let Some(base) = feed::nuget_base_address(&index) else {
        bail!(
            "the NuGet service index at {} has no PackageBaseAddress resource",
            feed::host(&index_url)
        );
    };
    let url = feed::nuget_versions_url(&base, package);
    Ok(match session.get(Ecosystem::Nuget, &url)? {
        Outcome::Body(body) => Fetched::Found {
            feed: index_url,
            versions: feed::nuget_versions(&body),
        },
        Outcome::NotFound => Fetched::NotFound { feed: index_url },
        Outcome::Refused { had_token } => Fetched::Refused { had_token },
    })
}

pub(crate) fn fetch(
    session: &Session,
    ecosystem: Ecosystem,
    package: &str,
    org: Option<&str>,
    registry: Option<&str>,
) -> Result<Fetched> {
    match ecosystem {
        Ecosystem::Npm => fetch_npm(session, package, org, registry),
        Ecosystem::Nuget => fetch_nuget(session, package, org, registry),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Published,
    Absent,
    Failed(String),
}

pub struct Checker {
    session: Session,
    registry: Option<String>,
    memo: BTreeMap<(String, String), Check>,
}

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn short_reason(error: &anyhow::Error) -> String {
    let text = error.to_string();
    let line = text.lines().next().unwrap_or_default();
    match line.split_once(". ") {
        Some((first, _)) => first.to_string(),
        None => line.trim_end_matches('.').to_string(),
    }
}

impl Checker {
    pub fn new() -> Self {
        Self::with_registry(None)
    }

    pub fn with_registry(registry: Option<String>) -> Self {
        Self {
            session: Session::new(),
            registry,
            memo: BTreeMap::new(),
        }
    }

    pub fn check(&mut self, package: &str, version: &str, org: Option<&str>) -> Check {
        let key = (package.to_string(), version.to_string());
        if let Some(known) = self.memo.get(&key) {
            return known.clone();
        }
        let outcome = self.lookup(package, version, org);
        self.memo.insert(key, outcome.clone());
        outcome
    }

    fn lookup(&self, package: &str, version: &str, org: Option<&str>) -> Check {
        let ecosystem = Ecosystem::default_for(package);
        if ecosystem == Ecosystem::Nuget && org.is_none() && self.registry.is_none() {
            return Check::Failed("no producer PR names the org that publishes it".to_string());
        }
        match fetch(
            &self.session,
            ecosystem,
            package,
            org,
            self.registry.as_deref(),
        ) {
            Err(error) => Check::Failed(short_reason(&error)),
            Ok(Fetched::Refused { .. }) => {
                Check::Failed("the feed refused the request (401/403): needs read:packages".into())
            }
            Ok(Fetched::NotFound { feed }) => Check::Failed(feed::not_found_message(&feed)),
            Ok(Fetched::Found { feed, versions }) => {
                let report = Report::build(package, ecosystem, &feed, Some(version), &versions);
                if report.published == Some(true) {
                    Check::Published
                } else {
                    Check::Absent
                }
            }
        }
    }
}
