use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use ureq::http::Response;
use ureq::{Agent, Body};

use super::error::ApiError;
use super::guard::Guard;
use super::reply::{Rate, Reply};
use crate::cache::Cache;
use crate::config::ApiConfig;

const API: &str = "https://api.github.com";

const MAX_HOPS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Download {
    Api(ApiError),
    TooLarge,
}

pub fn origin(url: &str) -> &str {
    let rest = url
        .split_once("://")
        .map_or(0, |(scheme, _)| scheme.len() + 3);
    match url[rest..].find('/') {
        Some(end) => &url[..rest + end],
        None => url,
    }
}

fn same_origin(left: &str, right: &str) -> bool {
    origin(left).eq_ignore_ascii_case(origin(right))
}

pub struct Client {
    agent: Agent,
    token: String,
    base: String,
    guard: Guard,
    pub(super) cache: Cache,
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
        Self::with_timeout(token, cfg, verbose, base, Duration::from_secs(40))
    }

    pub fn with_timeout(
        token: String,
        cfg: &ApiConfig,
        verbose: bool,
        base: &str,
        timeout: Duration,
    ) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout))
            .user_agent(concat!("pipetrain/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            token,
            base: base.trim_end_matches('/').to_string(),
            guard: Guard::new(cfg, verbose),
            cache: Cache::disabled(),
        }
    }

    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = cache;
        self
    }

    pub fn cache_hits(&self) -> u32 {
        self.cache.hits()
    }

    pub fn cache_misses(&self) -> u32 {
        self.cache.misses()
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
        let mut request = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if !self.token.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.token));
        }
        match request.call() {
            Ok(response) => Ok(self.read(response)),
            Err(error) => Err(transport(error, &format!("GET {path}")).redacted(&self.token)),
        }
    }

    pub fn download(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, Download> {
        let mut current = url.to_string();
        for _ in 0..=MAX_HOPS {
            self.guard
                .before("core", &format!("GET {current}"))
                .map_err(Download::Api)?;
            let mut request = self.agent.get(&current);
            if !self.token.is_empty() && same_origin(&current, &self.base) {
                request = request.header("Authorization", format!("Bearer {}", self.token));
            }
            let mut response = request.call().map_err(|error| {
                Download::Api(transport(error, &format!("GET {current}")).redacted(&self.token))
            })?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                let Some(next) = location
                    .filter(|next| next.starts_with("https://") || same_origin(next, &self.base))
                else {
                    return Err(Download::Api(ApiError::Other {
                        message: format!(
                            "GitHub redirected {current} somewhere that is not trusted"
                        ),
                    }));
                };
                current = next;
                continue;
            }
            if !(200..300).contains(&status) {
                let reply = Reply {
                    status,
                    rate: Rate::from_headers(response.headers()),
                    body: response.body_mut().read_json().unwrap_or(Value::Null),
                };
                let error = reply.into_body(&current).err().unwrap_or(ApiError::Other {
                    message: format!("GitHub answered {status} for {current}"),
                });
                return Err(Download::Api(error.redacted(&self.token)));
            }
            return match response.body_mut().with_config().limit(limit).read_to_vec() {
                Ok(bytes) => Ok(bytes),
                Err(ureq::Error::BodyExceedsLimit(_)) => Err(Download::TooLarge),
                Err(error) => Err(Download::Api(transport(error, &format!("GET {current}")))),
            };
        }
        Err(Download::Api(ApiError::Other {
            message: format!("too many redirects for {url}"),
        }))
    }

    pub(super) fn redact(&self, error: ApiError) -> ApiError {
        error.redacted(&self.token)
    }

    fn finish(
        &mut self,
        result: Result<Response<Body>, ureq::Error>,
        method: &str,
        path: &str,
    ) -> Result<Reply, ApiError> {
        match result {
            Ok(response) => Ok(self.read(response)),
            Err(error) => Err(transport(error, &format!("{method} {path}")).redacted(&self.token)),
        }
    }

    pub fn post_json(&mut self, path: &str, body: &Value) -> Result<Reply, ApiError> {
        self.guard.before("core", &format!("POST {path}"))?;
        let result = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body);
        self.finish(result, "POST", path)
    }

    pub fn patch_json(&mut self, path: &str, body: &Value) -> Result<Reply, ApiError> {
        self.guard.before("core", &format!("PATCH {path}"))?;
        let result = self
            .agent
            .patch(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body);
        self.finish(result, "PATCH", path)
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
