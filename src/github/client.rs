use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use ureq::http::Response;
use ureq::{Agent, Body};

use super::error::ApiError;
use super::guard::{Below, Guard, Usage};
use super::reply::{Rate, Reply};
use super::retry::Retry;
use crate::cache::{Cache, Freshness, HttpCache, PR_TTL, Stored, freshness_of, http_key};
use crate::config::{ApiConfig, Reserve};

type Sent = Result<Response<Body>, ureq::Error>;

fn cut_short_search(body: &Value) -> bool {
    let Some(search) = body.pointer("/data/search") else {
        return false;
    };
    let total = search.get("issueCount").and_then(Value::as_u64);
    let nodes = search
        .get("nodes")
        .and_then(Value::as_array)
        .map_or(0, |nodes| nodes.len() as u64);
    let more = search
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    !more && total.is_some_and(|total| nodes < total)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub limit: u64,
    pub remaining: u64,
    pub reset: Option<i64>,
}

const API: &str = "https://api.github.com";

const MAX_HOPS: usize = 5;

pub const API_ENV: &str = "PIPETRAIN_API_URL";

pub const QUOTA_CHECK_ENV: &str = "PIPETRAIN_QUOTA_CHECK";

pub fn is_loopback(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("http")
        && uri
            .authority()
            .is_some_and(|authority| !authority.as_str().contains('@'))
        && matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

pub(crate) fn debug_loopback_env(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok().filter(|url| is_loopback(url))
    } else {
        None
    }
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
    validators: Validators,
    http: HttpCache,
    live: bool,
    bypass_http: bool,
    pub(super) cache: Cache,
}

#[derive(Default, Clone)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

fn header_text(response: &Response<Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
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

    pub fn fork(&self) -> Self {
        Self {
            agent: self.agent.clone(),
            token: self.token.clone(),
            base: self.base.clone(),
            guard: self.guard.clone(),
            retry: self.retry,
            retry_after: None,
            validators: Validators::default(),
            http: self.http.clone(),
            live: self.live,
            bypass_http: self.bypass_http,
            cache: Cache::disabled(),
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
            validators: Validators::default(),
            http: HttpCache::off(),
            live: false,
            bypass_http: false,
            cache: Cache::disabled(),
        }
    }

    pub fn with_cache(self, cache: Cache) -> Self {
        self.with_profile_cache(cache, "")
    }

    pub fn with_profile_cache(mut self, cache: Cache, profile: &str) -> Self {
        self.cache = cache.for_github(&self.base, profile, &self.token);
        if self.http.enabled() {
            self.http = self.cache.http();
        }
        self
    }

    pub fn with_http_cache(mut self) -> Self {
        self.http = self.cache.http();
        self
    }

    pub(super) fn bypass_http(&mut self, bypass: bool) {
        self.bypass_http = bypass;
    }

    pub fn set_live(&mut self, live: bool) {
        self.live = live;
    }

    pub fn usage(&self) -> Usage {
        self.guard.usage()
    }

    pub fn below_reserve(&self) -> Option<Below> {
        self.guard.below_reserve()
    }

    pub fn reserve(&self) -> Reserve {
        self.guard.reserve()
    }

    pub fn sequence_remote(&self) -> Option<crate::sequence::Remote> {
        crate::sequence::Remote::resolve(&self.token, is_loopback(&self.base))
    }

    pub fn quota_probe(&mut self) -> Option<BTreeMap<String, Window>> {
        if is_loopback(&self.base) && std::env::var_os(QUOTA_CHECK_ENV).is_none() {
            return None;
        }
        self.rate_limits()
    }

    pub fn rate_limits(&mut self) -> Option<BTreeMap<String, Window>> {
        let url = format!("{}/rate_limit", self.base);
        let mut request = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if !self.token.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.token));
        }
        let mut response = request.call().ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        let body: Value = response.body_mut().read_json().ok()?;
        let mut windows = BTreeMap::new();
        for (name, entry) in body.get("resources")?.as_object()? {
            let number = |field: &str| entry.get(field).and_then(Value::as_u64);
            let (Some(limit), Some(remaining)) = (number("limit"), number("remaining")) else {
                continue;
            };
            let reset = entry.get("reset").and_then(Value::as_i64);
            self.guard.observe(name, remaining, reset, Some(limit));
            windows.insert(
                name.clone(),
                Window {
                    limit,
                    remaining,
                    reset,
                },
            );
        }
        Some(windows)
    }

    pub(super) fn pause_before_search_retry(&self, retried: u32) {
        if let Some(wait) = self.retry.wait(retried, None) {
            sleep(wait);
        }
    }

    pub fn cache_hits(&self) -> u32 {
        self.cache
            .hits()
            .saturating_add(self.guard.usage().cache_hits)
    }

    pub fn cache_misses(&self) -> u32 {
        self.cache.misses()
    }

    pub fn requests(&self) -> u32 {
        self.guard.total()
    }

    pub fn remaining(&self) -> Option<u64> {
        self.guard.remaining()
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
        self.validators = Validators {
            etag: header_text(&response, "etag"),
            last_modified: header_text(&response, "last-modified"),
        };
        if let Some(left) = rate.remaining {
            self.guard
                .observe(&rate.resource, left, rate.reset, rate.limit);
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

    fn cached(
        &mut self,
        resource: &str,
        label: &str,
        key: Option<(String, Freshness)>,
        send: &dyn Fn(&Agent, &Validators) -> Sent,
    ) -> Result<Reply, ApiError> {
        let stored = key
            .as_ref()
            .filter(|_| !self.bypass_http)
            .and_then(|(key, _)| self.http.load(key))
            .filter(|stored| stored.body != Value::Null);
        if let (Some(stored), Some((_, freshness))) = (&stored, &key)
            && stored.is_fresh(*freshness, self.live)
        {
            self.guard.hit();
            return Ok(Reply {
                status: 200,
                rate: Rate::unknown(resource),
                body: stored.body.clone(),
            });
        }
        let known = Validators {
            etag: stored.as_ref().and_then(|stored| stored.etag.clone()),
            last_modified: stored
                .as_ref()
                .and_then(|stored| stored.last_modified.clone()),
        };
        let mut reply = self.call_read(resource, label, &|agent| send(agent, &known))?;
        if let (304, Some(stored), Some((key, _))) = (reply.status, &stored, &key) {
            self.guard.refund(resource);
            let mut renewed = stored.clone();
            renewed.stored_at = crate::cache::now();
            self.http.save(key, &renewed);
            reply.status = 200;
            reply.body = stored.body.clone();
        } else if let Some((key, freshness)) = &key
            && (200..300).contains(&reply.status)
            && reply.body.get("errors").is_none()
            && !cut_short_search(&reply.body)
            && (*freshness != Freshness::Revalidate
                || self.validators.etag.is_some()
                || self.validators.last_modified.is_some())
        {
            self.http.save(
                key,
                &Stored::new(
                    self.validators.etag.clone(),
                    self.validators.last_modified.clone(),
                    reply.body.clone(),
                ),
            );
        }
        Ok(reply)
    }

    pub fn get(&mut self, path: &str) -> Result<Reply, ApiError> {
        let url = format!("{}{path}", self.base);
        let route = path.split('?').next().unwrap_or(path);
        let pr_files = route.ends_with("/files") && route.contains("/pulls/");
        let key = (self.http.enabled() && !pr_files).then(|| {
            (
                http_key("GET", &url, "", &self.token),
                freshness_of(path, self.live),
            )
        });
        let token = self.token.clone();
        self.cached("core", &format!("GET {path}"), key, &move |agent, known| {
            let mut request = agent
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if !token.is_empty() {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            if let Some(etag) = &known.etag {
                request = request.header("If-None-Match", etag);
            }
            if let Some(since) = &known.last_modified {
                request = request.header("If-Modified-Since", since);
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
        let key = (self.http.enabled() && !self.live).then(|| {
            (
                http_key(
                    "POST",
                    &url,
                    &format!("{query}\u{0}{variables}"),
                    &self.token,
                ),
                Freshness::Ttl(PR_TTL),
            )
        });
        let body = json!({ "query": query, "variables": variables });
        self.cached(
            "graphql",
            &format!("GRAPHQL {label}"),
            key,
            &move |agent, _| {
                agent
                    .post(&url)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Accept", "application/vnd.github+json")
                    .send_json(&body)
            },
        )
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
