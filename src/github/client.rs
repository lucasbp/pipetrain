use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use ureq::Agent;

use super::guard::Guard;
use crate::config::ApiConfig;

const API: &str = "https://api.github.com";

pub struct Client {
    agent: Agent,
    token: String,
    guard: Guard,
}

impl Client {
    pub fn new(token: String, cfg: &ApiConfig, verbose: bool) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(40)))
            .user_agent(concat!("pipetrain/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            token,
            guard: Guard::new(cfg, verbose),
        }
    }

    pub fn requests(&self) -> u32 {
        self.guard.total
    }

    pub fn remaining(&self) -> Option<u64> {
        self.guard.remaining.values().copied().min()
    }

    fn record_rate_limit(&mut self, response: &ureq::http::Response<ureq::Body>) {
        if let Some(left) = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            let resource = response
                .headers()
                .get("x-ratelimit-resource")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("core")
                .to_string();
            self.guard.remaining.insert(resource, left);
        }
    }

    fn finish(
        &mut self,
        mut response: ureq::http::Response<ureq::Body>,
        label: &str,
    ) -> Result<Value> {
        let status = response.status().as_u16();
        self.record_rate_limit(&response);
        let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        check_status(status, &body, label)?;
        Ok(body)
    }

    pub fn rest(&mut self, path: &str) -> Result<Value> {
        self.guard.before("core", &format!("GET {path}"))?;
        let response = self
            .agent
            .get(format!("{API}{path}"))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .with_context(|| format!("request failed: GET {path}"))?;
        self.finish(response, path)
    }

    pub fn graphql(&mut self, label: &str, query: &str, variables: Value) -> Result<Value> {
        self.guard.before("graphql", &format!("GRAPHQL {label}"))?;
        let response = self
            .agent
            .post(format!("{API}/graphql"))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .send_json(json!({ "query": query, "variables": variables }))
            .with_context(|| format!("request failed: GRAPHQL {label}"))?;
        let body = self.finish(response, label)?;
        check_graphql_errors(&body, label)?;
        Ok(body)
    }
}

fn check_status(status: u16, body: &Value, label: &str) -> Result<()> {
    if (300..400).contains(&status) {
        bail!(
            "GitHub redirected {label}: the repo was probably renamed or moved; update the reference"
        );
    }
    if status < 400 {
        return Ok(());
    }
    let message = body
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("no message");
    Err(match status {
        401 => anyhow!("GitHub rejected the token (401): run `gh auth login` again"),
        403 | 429 => anyhow!(
            "GitHub refused {label} ({status}): {message}; this can be a rate limit, wait before retrying"
        ),
        404 => anyhow!("not found: {label} (404); check the name and that the token can read it"),
        _ => anyhow!("GitHub answered {status} for {label}: {message}"),
    })
}

fn check_graphql_errors(body: &Value, label: &str) -> Result<()> {
    let Some(errors) = body.get("errors").and_then(|e| e.as_array()) else {
        return Ok(());
    };
    let only_not_found = errors
        .iter()
        .all(|e| e.get("type").and_then(|t| t.as_str()) == Some("NOT_FOUND"));
    if !only_not_found || body.get("data").is_none_or(|d| d.is_null()) {
        let message = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("GraphQL error in {label}: {message}");
    }
    Ok(())
}
