use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use ureq::http::Response;
use ureq::{Agent, Body};

use super::error::ApiError;
use super::guard::Guard;
use super::reply::{Rate, Reply};
use crate::config::ApiConfig;

const API: &str = "https://api.github.com";

pub struct Client {
    agent: Agent,
    token: String,
    base: String,
    guard: Guard,
}

fn transport(error: ureq::Error, label: &str) -> ApiError {
    match error {
        ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Io(_) => ApiError::Network,
        other => ApiError::Other {
            message: format!("request failed: {label}: {other}"),
        },
    }
}

impl Client {
    pub fn new(token: String, cfg: &ApiConfig, verbose: bool) -> Self {
        Self::with_base_url(token, cfg, verbose, API)
    }

    #[doc(hidden)]
    pub fn with_base_url(token: String, cfg: &ApiConfig, verbose: bool, base: &str) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(40)))
            .user_agent(concat!("pipetrain/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            token,
            base: base.trim_end_matches('/').to_string(),
            guard: Guard::new(cfg, verbose),
        }
    }

    pub fn requests(&self) -> u32 {
        self.guard.total
    }

    pub fn remaining(&self) -> Option<u64> {
        self.guard.remaining.values().copied().min()
    }

    fn read(&mut self, mut response: Response<Body>) -> Reply {
        let status = response.status().as_u16();
        let rate = Rate::from_headers(response.headers());
        if let Some(left) = rate.remaining {
            self.guard.remaining.insert(rate.resource.clone(), left);
        }
        let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        Reply { status, rate, body }
    }

    pub fn get(&mut self, path: &str) -> Result<Reply, ApiError> {
        self.guard.before("core", &format!("GET {path}"))?;
        let result = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call();
        match result {
            Ok(response) => Ok(self.read(response)),
            Err(error) => Err(transport(error, &format!("GET {path}")).redacted(&self.token)),
        }
    }

    pub fn query(&mut self, label: &str, query: &str, variables: Value) -> Result<Reply, ApiError> {
        self.guard.before("graphql", &format!("GRAPHQL {label}"))?;
        let result = self
            .agent
            .post(format!("{}/graphql", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .send_json(json!({ "query": query, "variables": variables }));
        match result {
            Ok(response) => Ok(self.read(response)),
            Err(error) => Err(transport(error, &format!("GRAPHQL {label}")).redacted(&self.token)),
        }
    }

    pub fn rest(&mut self, path: &str) -> Result<Value> {
        let reply = self.get(path)?;
        Ok(reply
            .into_body(path)
            .map_err(|error| error.redacted(&self.token))?)
    }

    pub fn graphql(&mut self, label: &str, query: &str, variables: Value) -> Result<Value> {
        let reply = self.query(label, query, variables)?;
        Ok(reply
            .into_graphql(label)
            .map_err(|error| error.redacted(&self.token))?)
    }
}
