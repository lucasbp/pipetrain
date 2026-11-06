use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use ureq::http::Response;
use ureq::{Agent, Body};

use super::error::ApiError;
use super::guard::Guard;
use super::reply::{Rate, Reply};
use super::retry::Retry;
use crate::cache::Cache;
use crate::config::ApiConfig;

const API: &str = "https://api.github.com";

const MAX_HOPS: usize = 5;

pub const API_ENV: &str = "PIPETRAIN_API_URL";

pub fn is_loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = if authority.starts_with('[') {
        authority.split(']').next().map(|h| format!("{h}]"))
    } else {
        authority.split(':').next().map(str::to_string)
    };
    matches!(host.as_deref(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

fn api_override(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|url| is_loopback(url))
}

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
    retry: Retry,
    retry_after: Option<Duration>,
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
        let env = std::env::var(API_ENV).ok();
        let base = api_override(env.as_deref());
        let client = Self::with_base_url(token, cfg, verbose, base.unwrap_or(API));
        if base.is_some() {
            client
        } else {
            client.with_retry(Retry::standard())
        }
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
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
            retry: Retry::off(),
            retry_after: None,
            cache: Cache::disabled(),
        }
    }

    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = cache;
        self
    }

    pub(super) fn pause_before_search_retry(&self, retried: u32) {
        if let Some(wait) = self.retry.wait(retried, None) {
            sleep(wait);
        }
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
        self.retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        if let Some(left) = rate.remaining {
            self.guard.remaining.insert(rate.resource.clone(), left);
        }
        let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        Reply { status, rate, body }
    }

    fn call_read(
        &mut self,
        resource: &str,
        label: &str,
        send: &dyn Fn(&Agent) -> Result<Response<Body>, ureq::Error>,
    ) -> Result<Reply, ApiError> {
        let mut retried = 0;
        loop {
            self.guard.before(resource, label)?;
            let reply = match send(&self.agent) {
                Ok(response) => self.read(response),
                Err(error) => return Err(transport(error, label).redacted(&self.token)),
            };
            let Some(cause) = Retry::cause(&reply, self.retry_after) else {
                return Ok(reply);
            };
            let Some(wait) = self.retry.wait(retried, self.retry_after) else {
                return Ok(reply);
            };
            retried += 1;
            crate::progress::line(&format!(
                "GitHub {} for {label}: retrying in {:.1}s (retry {retried} of {})",
                cause.describe(),
                wait.as_secs_f64(),
                self.retry.max_retries
            ));
            sleep(wait);
        }
    }

    pub fn get(&mut self, path: &str) -> Result<Reply, ApiError> {
        let url = format!("{}{path}", self.base);
        let token = self.token.clone();
        self.call_read("core", &format!("GET {path}"), &move |agent| {
            let mut request = agent
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if !token.is_empty() {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            request.call()
        })
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

    pub fn put_json(&mut self, path: &str, body: &Value) -> Result<Reply, ApiError> {
        self.guard.before("core", &format!("PUT {path}"))?;
        let result = self
            .agent
            .put(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body);
        self.finish(result, "PUT", path)
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
        let url = format!("{}/graphql", self.base);
        let token = self.token.clone();
        let body = json!({ "query": query, "variables": variables });
        self.call_read("graphql", &format!("GRAPHQL {label}"), &move |agent| {
            agent
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/vnd.github+json")
                .send_json(&body)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_urls_override_the_api() {
        for ok in [
            "http://127.0.0.1:8080",
            "http://localhost:1",
            "http://[::1]:9/x",
            " http://localhost ",
        ] {
            assert!(api_override(Some(ok)).is_some(), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:8080",
            "http://example.com",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "http://evil.com/127.0.0.1",
            "",
        ] {
            assert!(api_override(Some(bad)).is_none(), "{bad}");
        }
        assert!(api_override(None).is_none());
    }
}
