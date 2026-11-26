use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;
use ureq::Agent;

use crate::feed::{self, Coordinate, Ecosystem, Report};
use crate::{github, hint};

mod registries;
pub(crate) use registries::{exclude_go_retractions, validate_version};

const NPM_ACCEPT: &str = "application/vnd.npm.install-v1+json";

enum Outcome<T = Value> {
    Body(T),
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
        match ecosystem {
            Ecosystem::Npm => self.token().map(feed::bearer),
            Ecosystem::Nuget => self.token().map(feed::basic),
            _ => None,
        }
    }

    fn get(&self, ecosystem: Ecosystem, url: &str) -> Result<Outcome> {
        self.get_body(ecosystem, url, |body| {
            body.read_json()
                .map_err(|_| anyhow::anyhow!("{} did not answer with valid JSON", feed::host(url)))
        })
    }

    fn get_body<T>(
        &self,
        ecosystem: Ecosystem,
        url: &str,
        decode: impl FnOnce(&mut ureq::Body) -> Result<T>,
    ) -> Result<Outcome<T>> {
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
            200..=299 => decode(response.body_mut()).map(Outcome::Body),
            404 => Ok(Outcome::NotFound),
            410 if ecosystem == Ecosystem::Go => Ok(Outcome::NotFound),
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

pub(crate) fn fetch_nuget(
    session: &Session,
    package: &str,
    org: Option<&str>,
    registry: Option<&str>,
    inspect: impl FnOnce(&str, &Value) -> Result<()>,
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
    inspect(&index_url, &index)?;
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
    version: Option<&str>,
    org: Option<&str>,
    registry: Option<&str>,
) -> Result<Fetched> {
    registries::validate_version(ecosystem, version)?;
    match ecosystem {
        Ecosystem::Npm => fetch_npm(session, package, org, registry),
        Ecosystem::Nuget => fetch_nuget(session, package, org, registry, |_, _| Ok(())),
        Ecosystem::Cargo | Ecosystem::Pypi | Ecosystem::Go => {
            registries::fetch(session, ecosystem, package, version, registry)
        }
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
    memo: BTreeMap<(Coordinate, Option<String>), Check>,
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
        let coordinate = Coordinate {
            ecosystem: Ecosystem::default_for(package),
            package: package.to_string(),
            version: version.to_string(),
        };
        self.check_coordinate(&coordinate, org)
    }

    pub(crate) fn check_coordinate(&mut self, coordinate: &Coordinate, org: Option<&str>) -> Check {
        let key = (coordinate.clone(), org.map(str::to_owned));
        if let Some(known) = self.memo.get(&key) {
            return known.clone();
        }
        let outcome = self.lookup(coordinate, org);
        self.memo.insert(key, outcome.clone());
        outcome
    }

    fn lookup(&self, coordinate: &Coordinate, org: Option<&str>) -> Check {
        let ecosystem = coordinate.ecosystem;
        let package = coordinate.package.as_str();
        let version = coordinate.version.as_str();
        if ecosystem == Ecosystem::Nuget && org.is_none() && self.registry.is_none() {
            return Check::Failed("no producer PR names the org that publishes it".to_string());
        }
        match fetch(
            &self.session,
            ecosystem,
            package,
            Some(version),
            org,
            self.registry.as_deref(),
        ) {
            Err(error) => Check::Failed(short_reason(&error)),
            Ok(Fetched::Refused { .. })
                if matches!(ecosystem, Ecosystem::Nuget | Ecosystem::Npm) =>
            {
                Check::Failed("the feed refused the request (401/403): needs read:packages".into())
            }
            Ok(Fetched::Refused { .. }) => {
                Check::Failed("the registry refused the request (401/403)".into())
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
