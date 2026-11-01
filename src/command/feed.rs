use std::cell::OnceCell;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;
use ureq::Agent;

use crate::feed::{self, Ecosystem, Report};
use crate::github;
use crate::hint;

const NPM_ACCEPT: &str = "application/vnd.npm.install-v1+json";

pub struct FeedOptions {
    pub package: String,
    pub version: Option<String>,
    pub ecosystem: Option<Ecosystem>,
    pub org: Option<String>,
    pub registry: Option<String>,
    pub json: bool,
}

enum Outcome {
    Body(Value),
    NotFound,
    Refused { had_token: bool },
}

struct Session {
    agent: Agent,
    token: OnceCell<Option<String>>,
}

impl Session {
    fn new() -> Self {
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

enum Fetched {
    Found { feed: String, versions: Vec<String> },
    NotFound { feed: String },
    Refused { had_token: bool },
}

fn fetch_npm(session: &Session, opts: &FeedOptions) -> Result<Fetched> {
    let base = feed::npm_base(&opts.package, opts.org.as_deref(), opts.registry.as_deref());
    let url = feed::npm_url(&base, &opts.package);
    Ok(match session.get(Ecosystem::Npm, &url)? {
        Outcome::Body(body) => Fetched::Found {
            feed: base,
            versions: feed::npm_versions(&body),
        },
        Outcome::NotFound => Fetched::NotFound { feed: base },
        Outcome::Refused { had_token } => Fetched::Refused { had_token },
    })
}

fn fetch_nuget(session: &Session, opts: &FeedOptions) -> Result<Fetched> {
    let Some(index_url) = feed::nuget_index_url(opts.org.as_deref(), opts.registry.as_deref())
    else {
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
    let url = feed::nuget_versions_url(&base, &opts.package);
    Ok(match session.get(Ecosystem::Nuget, &url)? {
        Outcome::Body(body) => Fetched::Found {
            feed: index_url,
            versions: feed::nuget_versions(&body),
        },
        Outcome::NotFound => Fetched::NotFound { feed: index_url },
        Outcome::Refused { had_token } => Fetched::Refused { had_token },
    })
}

fn lookup(opts: &FeedOptions) -> Result<Result<Report, i32>> {
    let ecosystem = opts
        .ecosystem
        .unwrap_or_else(|| Ecosystem::default_for(&opts.package));
    let session = Session::new();
    let fetched = match ecosystem {
        Ecosystem::Npm => fetch_npm(&session, opts)?,
        Ecosystem::Nuget => fetch_nuget(&session, opts)?,
    };
    match fetched {
        Fetched::Refused { had_token } => {
            eprintln!("{}", feed::refused_message(had_token));
            Ok(Err(feed::EXIT_REFUSED))
        }
        Fetched::NotFound { feed } => {
            eprintln!("{}", feed::not_found_message(&feed));
            Ok(Err(feed::EXIT_NOT_PUBLISHED))
        }
        Fetched::Found { feed, versions } => Ok(Ok(Report::build(
            &opts.package,
            ecosystem,
            &feed,
            opts.version.as_deref(),
            &versions,
        ))),
    }
}

pub fn latest(opts: &FeedOptions) -> Result<Result<String, i32>> {
    Ok(match lookup(opts)? {
        Ok(report) => match report.latest {
            Some(latest) => Ok(latest),
            None => {
                eprintln!("{}", feed::not_found_message(&report.feed));
                Err(feed::EXIT_NOT_PUBLISHED)
            }
        },
        Err(code) => Err(code),
    })
}

pub fn run(opts: FeedOptions) -> Result<i32> {
    match lookup(&opts)? {
        Err(code) => Ok(code),
        Ok(report) => {
            if opts.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&feed::render_json(&report))?
                );
            } else {
                println!("{}", feed::render_text(&report));
            }
            Ok(report.exit_code())
        }
    }
}
