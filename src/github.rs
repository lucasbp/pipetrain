use std::collections::{BTreeMap, VecDeque};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use ureq::Agent;

use crate::config::ApiConfig;
use crate::detect;
use crate::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use crate::planner::DepState;

const API: &str = "https://api.github.com";
const FILE_PAGES: u32 = 3;

pub fn token() -> Result<String> {
    if let Ok(out) = Command::new("gh").args(["auth", "token"]).output() {
        if out.status.success() {
            let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    match std::env::var("GH_TOKEN") {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => bail!("no GitHub token: run `gh auth login`, or set GH_TOKEN"),
    }
}

struct Guard {
    max_total: u32,
    max_per_minute: u32,
    min_remaining: u64,
    total: u32,
    stamps: VecDeque<Instant>,
    remaining: BTreeMap<String, u64>,
    verbose: bool,
}

impl Guard {
    fn new(cfg: &ApiConfig, verbose: bool) -> Self {
        Self {
            max_total: cfg.max_requests,
            max_per_minute: cfg.max_per_minute.max(1),
            min_remaining: cfg.min_remaining,
            total: 0,
            stamps: VecDeque::new(),
            remaining: BTreeMap::new(),
            verbose,
        }
    }

    fn before(&mut self, resource: &str, label: &str) -> Result<()> {
        if self.total >= self.max_total {
            bail!(
                "stopped after {} API requests (api.max_requests); raise it in the config if this is expected",
                self.total
            );
        }
        if let Some(&left) = self.remaining.get(resource) {
            if left < self.min_remaining {
                bail!(
                    "stopped: only {left} GitHub {resource} API requests left in this window (api.min_remaining is {})",
                    self.min_remaining
                );
            }
        }
        let window = Duration::from_secs(60);
        while self.stamps.front().is_some_and(|t| t.elapsed() >= window) {
            self.stamps.pop_front();
        }
        if self.stamps.len() >= self.max_per_minute as usize {
            if let Some(oldest) = self.stamps.front() {
                let wait = window.saturating_sub(oldest.elapsed());
                if self.verbose {
                    eprintln!("pacing: waiting {}s", wait.as_secs() + 1);
                }
                sleep(wait + Duration::from_millis(250));
            }
            self.stamps.pop_front();
        }
        self.stamps.push_back(Instant::now());
        self.total += 1;
        if self.verbose {
            eprintln!("request {:>3}  {label}", self.total);
        }
        Ok(())
    }
}

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

    fn finish(
        &mut self,
        mut response: ureq::http::Response<ureq::Body>,
        label: &str,
    ) -> Result<Value> {
        let status = response.status().as_u16();
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
        let body: Value = response
            .body_mut()
            .read_json()
            .unwrap_or_else(|_| Value::Null);
        if (300..400).contains(&status) {
            bail!(
                "GitHub redirected {label}: the repo was probably renamed or moved; update the reference"
            );
        }
        if status >= 400 {
            let message = body
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("no message");
            return Err(match status {
                401 => anyhow!("GitHub rejected the token (401): run `gh auth login` again"),
                403 | 429 => anyhow!(
                    "GitHub refused {label} ({status}): {message}; this can be a rate limit, wait before retrying"
                ),
                404 => anyhow!(
                    "not found: {label} (404); check the name and that the token can read it"
                ),
                _ => anyhow!("GitHub answered {status} for {label}: {message}"),
            });
        }
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
        if let Some(errors) = body.get("errors").and_then(|e| e.as_array()) {
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
        }
        Ok(body)
    }
}

const PR_FIELDS: &str = "number title url body headRefName baseRefName headRefOid isDraft state reviewDecision mergeable repository { name owner { login } } latestOpinionatedReviews(first: 20, writersOnly: true) { nodes { state } } commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }";

pub fn parse_pr(node: &Value) -> Option<Pr> {
    let number = node.get("number")?.as_u64()?;
    let repo = node.get("repository")?;
    let org = repo.get("owner")?.get("login")?.as_str()?;
    let name = repo.get("name")?.as_str()?;
    let text = |field: &str| {
        node.get(field)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let state = match node.get("state").and_then(|v| v.as_str()) {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ => PrState::Open,
    };
    let reviews: Vec<&str> = node
        .pointer("/latestOpinionatedReviews/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("state").and_then(|s| s.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let review = match node.get("reviewDecision").and_then(|v| v.as_str()) {
        Some("APPROVED") => Review::Approved,
        Some("CHANGES_REQUESTED") => Review::ChangesRequested,
        Some("REVIEW_REQUIRED") => Review::ReviewRequired,
        _ => {
            if reviews.contains(&"CHANGES_REQUESTED") {
                Review::ChangesRequested
            } else if reviews.contains(&"APPROVED") {
                Review::Approved
            } else {
                Review::None
            }
        }
    };
    let checks = match node
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(|v| v.as_str())
    {
        Some("SUCCESS") => Checks::Success,
        Some("FAILURE") | Some("ERROR") => Checks::Failure,
        Some(_) => Checks::Pending,
        None => Checks::Missing,
    };
    Some(Pr {
        key: PrKey::new(org, name, number),
        state,
        title: text("title"),
        url: text("url"),
        body: text("body"),
        head_ref: text("headRefName"),
        base_ref: text("baseRefName"),
        head_sha: text("headRefOid"),
        is_draft: node
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        review,
        checks,
        conflicting: node.get("mergeable").and_then(|v| v.as_str()) == Some("CONFLICTING"),
        mergeable_unknown: matches!(
            node.get("mergeable").and_then(|v| v.as_str()),
            None | Some("UNKNOWN")
        ),
        files: Vec::new(),
        files_complete: true,
    })
}

pub fn parse_search(response: &Value) -> Vec<Pr> {
    response
        .pointer("/data/search/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| nodes.iter().filter_map(parse_pr).collect())
        .unwrap_or_default()
}

pub fn parse_files(value: &Value) -> Vec<ChangedFile> {
    value
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    let path = f.get("filename")?.as_str()?.to_string();
                    let patch = if detect::keeps_patch(&path) {
                        f.get("patch").and_then(|p| p.as_str()).map(str::to_string)
                    } else {
                        None
                    };
                    Some(ChangedFile { path, patch })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Client {
    pub fn open_prs(&mut self, org: &str) -> Result<Vec<Pr>> {
        let query = format!(
            "query($q: String!, $after: String) {{ search(query: $q, type: ISSUE, first: 50, after: $after) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {PR_FIELDS} }} }} }} }}"
        );
        let search = format!("org:{org} is:pr is:open archived:false");
        let mut after = Value::Null;
        let mut all = Vec::new();
        loop {
            let response = self.graphql(
                &format!("search open PRs in {org}"),
                &query,
                json!({ "q": search, "after": after }),
            )?;
            all.extend(parse_search(&response));
            let page = response.pointer("/data/search/pageInfo");
            let more = page
                .and_then(|p| p.get("hasNextPage"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !more {
                break;
            }
            after = page
                .and_then(|p| p.get("endCursor"))
                .cloned()
                .unwrap_or(Value::Null);
        }
        Ok(all)
    }

    pub fn pr(&mut self, key: &PrKey) -> Result<Option<Pr>> {
        let query = format!(
            "query($o: String!, $r: String!, $n: Int!) {{ repository(owner: $o, name: $r) {{ pullRequest(number: $n) {{ {PR_FIELDS} }} }} }}"
        );
        let response = self.graphql(
            &format!("read {key}"),
            &query,
            json!({ "o": key.org, "r": key.repo, "n": key.number }),
        )?;
        Ok(response
            .pointer("/data/repository/pullRequest")
            .and_then(parse_pr))
    }

    pub fn dep_state(&mut self, key: &PrKey) -> Result<DepState> {
        Ok(match self.pr(key)? {
            None => DepState::Missing,
            Some(pr) => match pr.state {
                PrState::Merged => DepState::Merged,
                PrState::Closed => DepState::Closed,
                PrState::Open => DepState::Open,
            },
        })
    }

    pub fn files(&mut self, key: &PrKey) -> Result<(Vec<ChangedFile>, bool)> {
        let mut files = Vec::new();
        let mut complete = true;
        for page in 1..=FILE_PAGES {
            let value = self.rest(&format!(
                "/repos/{}/{}/pulls/{}/files?per_page=100&page={page}",
                key.org, key.repo, key.number
            ))?;
            let got = parse_files(&value);
            let full = value.as_array().is_some_and(|a| a.len() == 100);
            files.extend(got);
            if !full {
                break;
            }
            if page == FILE_PAGES {
                complete = false;
            }
        }
        Ok((files, complete))
    }

    pub fn tree(&mut self, slug: &str, sha: &str) -> Result<(Vec<String>, bool)> {
        let value = self.rest(&format!("/repos/{slug}/git/trees/{sha}?recursive=1"))?;
        let paths = value
            .get("tree")
            .and_then(|t| t.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("blob"))
                    .filter_map(|e| e.get("path").and_then(|p| p.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let truncated = value
            .get("truncated")
            .and_then(|t| t.as_bool())
            .unwrap_or(false);
        Ok((paths, truncated))
    }

    pub fn blobs(
        &mut self,
        slug: &str,
        sha: &str,
        paths: &[String],
    ) -> Result<Vec<(String, String)>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let (owner, name) = slug
            .split_once('/')
            .ok_or_else(|| anyhow!("bad repo slug {slug}"))?;
        let mut declarations = String::from("$o: String!, $r: String!");
        let mut selections = String::new();
        let mut variables = json!({ "o": owner, "r": name });
        for (i, path) in paths.iter().enumerate() {
            declarations.push_str(&format!(", $e{i}: String!"));
            selections.push_str(&format!(
                "f{i}: object(expression: $e{i}) {{ ... on Blob {{ text }} }} "
            ));
            variables[format!("e{i}")] = Value::String(format!("{sha}:{path}"));
        }
        let query = format!(
            "query({declarations}) {{ repository(owner: $o, name: $r) {{ {selections} }} }}"
        );
        let response = self.graphql(&format!("read manifests of {slug}"), &query, variables)?;
        let mut found = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            if let Some(text) = response
                .pointer(&format!("/data/repository/f{i}/text"))
                .and_then(|t| t.as_str())
            {
                found.push((path.clone(), text.to_string()));
            }
        }
        Ok(found)
    }
}
