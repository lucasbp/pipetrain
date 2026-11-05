use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::cached;
use super::client::Client;
use super::error::ApiError;
use super::parse::{parse_files, parse_pr, parse_search};
use crate::model::{ChangedFile, Pr, PrKey, PrState};
use crate::planner::DepState;

const FILE_PAGES: u32 = 3;
const BRANCH_PAGES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchWrite {
    Created,
    AlreadyExists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrWrite {
    Created,
    AlreadyExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePr {
    pub open: bool,
    pub merged: bool,
    pub head_sha: String,
    pub base: String,
    pub mergeable_state: String,
    pub merge_commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    pub workflow_id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
}

fn parse_run(value: &Value) -> Run {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let number = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or_default();
    Run {
        id: number("id"),
        workflow_id: number("workflow_id"),
        name: text("name"),
        status: text("status"),
        conclusion: value
            .get("conclusion")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: text("html_url"),
    }
}

fn encode_path(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(char::from(byte));
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

const PR_FIELDS: &str = "number title url body headRefName baseRefName baseRef { target { oid } } headRefOid isDraft state reviewDecision mergeable mergeStateStatus author { login } labels(first: 50) { nodes { name } } repository { name owner { login } } latestOpinionatedReviews(first: 20, writersOnly: true) { nodes { state } } commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 100) { nodes { __typename ... on CheckRun { name conclusion } ... on StatusContext { context state } } } } } } }";

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

    pub fn viewer_login(&mut self) -> Result<String> {
        let response = self.graphql(
            "read the token's login",
            "query { viewer { login } }",
            json!({}),
        )?;
        response
            .pointer("/data/viewer/login")
            .and_then(|v| v.as_str())
            .map(str::to_lowercase)
            .ok_or_else(|| anyhow!("GitHub did not return the login of the token"))
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

    pub fn repo_in_org(&mut self, org: &str, repo: &str) -> Result<Option<String>> {
        let path = format!("/repos/{org}/{repo}");
        let reply = self.get(&path)?;
        if reply.status == 404 {
            return Ok(None);
        }
        let body = reply.into_body(&path).map_err(|error| self.redact(error))?;
        let owner = body.pointer("/owner/login").and_then(|v| v.as_str());
        let name = body.get("name").and_then(|v| v.as_str());
        Ok(Some(match (owner, name) {
            (Some(owner), Some(name)) => format!("{owner}/{name}"),
            _ => format!("{org}/{repo}"),
        }))
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

    pub fn files(
        &mut self,
        key: &PrKey,
        head_sha: &str,
        base_sha: &str,
    ) -> Result<(Vec<ChangedFile>, bool)> {
        let slug = key.slug();
        let number = key.number.to_string();
        let parts = [number.as_str(), base_sha, head_sha];
        let usable = !head_sha.is_empty() && !base_sha.is_empty();
        if usable
            && let Some(found) = self
                .cache
                .get(&slug, "files", &parts)
                .and_then(|value| cached::files_from(&value))
        {
            return Ok(found);
        }
        let (files, complete) = self.fetch_files(key)?;
        if usable && cached::storable(&files) {
            let value = cached::files_value(&files, complete);
            self.cache.put(&slug, "files", &parts, &value);
        }
        Ok((files, complete))
    }

    fn fetch_files(&mut self, key: &PrKey) -> Result<(Vec<ChangedFile>, bool)> {
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

    pub fn default_branch_sha(&mut self, slug: &str) -> Result<String> {
        let (owner, name) = slug
            .split_once('/')
            .ok_or_else(|| anyhow!("bad repo slug {slug}"))?;
        let query = "query($o: String!, $r: String!) { repository(owner: $o, name: $r) { defaultBranchRef { target { oid } } } }";
        let response = self.graphql(
            &format!("read the default branch of {slug}"),
            query,
            json!({ "o": owner, "r": name }),
        )?;
        let sha = response
            .pointer("/data/repository/defaultBranchRef/target/oid")
            .and_then(|oid| oid.as_str());
        match sha {
            Some(oid) => Ok(oid.to_string()),
            None => Err(ApiError::NotFound {
                what: format!("the default branch of {slug}"),
            }
            .into()),
        }
    }

    pub fn branch_sha(&mut self, slug: &str, branch: &str) -> Result<String> {
        let value = self.rest(&format!(
            "/repos/{slug}/git/ref/heads/{}",
            encode_path(branch)
        ))?;
        match value.pointer("/object/sha").and_then(|sha| sha.as_str()) {
            Some(sha) => Ok(sha.to_string()),
            None => Err(ApiError::NotFound {
                what: format!("branch {branch} of {slug}"),
            }
            .into()),
        }
    }

    pub fn create_branch(
        &mut self,
        slug: &str,
        branch: &str,
        sha: &str,
    ) -> Result<BranchWrite, ApiError> {
        let path = format!("/repos/{slug}/git/refs");
        let body = json!({ "ref": format!("refs/heads/{branch}"), "sha": sha });
        let reply = self.post_json(&path, &body)?;
        if reply.status == 422
            && reply
                .message()
                .to_lowercase()
                .contains("reference already exists")
        {
            return Ok(BranchWrite::AlreadyExists);
        }
        reply
            .into_write(&format!("POST {path}"))
            .map_err(|error| self.redact(error))?;
        Ok(BranchWrite::Created)
    }

    pub fn branch_tip(&mut self, slug: &str, branch: &str) -> Result<Option<String>> {
        let path = format!("/repos/{slug}/git/ref/heads/{}", encode_path(branch));
        let reply = self.get(&path)?;
        if reply.status == 404 {
            return Ok(None);
        }
        let body = reply.into_body(&path).map_err(|error| self.redact(error))?;
        Ok(body
            .pointer("/object/sha")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    pub fn contains_commit(
        &mut self,
        slug: &str,
        branch_sha: &str,
        base_sha: &str,
    ) -> Result<bool> {
        if branch_sha == base_sha {
            return Ok(true);
        }
        let value = self.rest(&format!(
            "/repos/{slug}/compare/{base_sha}...{branch_sha}?per_page=1"
        ))?;
        Ok(matches!(
            value.get("status").and_then(Value::as_str),
            Some("ahead" | "identical")
        ))
    }

    pub fn open_pr_from(&mut self, slug: &str, branch: &str, base: &str) -> Result<Option<u64>> {
        let owner = slug.split('/').next().unwrap_or_default();
        let value = self.rest(&format!(
            "/repos/{slug}/pulls?state=open&head={}&base={}&per_page=1",
            encode_path(&format!("{owner}:{branch}")),
            encode_path(base)
        ))?;
        Ok(value
            .as_array()
            .and_then(|list| list.first())
            .and_then(|pr| pr.get("number"))
            .and_then(Value::as_u64))
    }

    pub fn create_pr(
        &mut self,
        slug: &str,
        title: &str,
        head: &str,
        base: &str,
        body: &str,
    ) -> Result<PrWrite, ApiError> {
        let path = format!("/repos/{slug}/pulls");
        let payload = json!({ "title": title, "head": head, "base": base, "body": body });
        let reply = self.post_json(&path, &payload)?;
        if reply.status == 422
            && reply
                .message()
                .to_lowercase()
                .contains("pull request already exists")
        {
            return Ok(PrWrite::AlreadyExists);
        }
        reply
            .into_write(&format!("POST {path}"))
            .map_err(|error| self.redact(error))?;
        Ok(PrWrite::Created)
    }

    pub fn branch_names(&mut self, slug: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for page in 1..=BRANCH_PAGES {
            let value = self.rest(&format!("/repos/{slug}/branches?per_page=100&page={page}"))?;
            let list = value.as_array().cloned().unwrap_or_default();
            names.extend(
                list.iter()
                    .filter_map(|branch| branch.get("name").and_then(Value::as_str))
                    .map(str::to_string),
            );
            if list.len() < 100 {
                break;
            }
        }
        Ok(names)
    }

    pub fn retarget_pr(&mut self, slug: &str, number: u64, branch: &str) -> Result<(), ApiError> {
        let path = format!("/repos/{slug}/pulls/{number}");
        let reply = self.patch_json(&path, &json!({ "base": branch }))?;
        reply
            .into_write(&format!("PATCH {path}"))
            .map_err(|error| self.redact(error))?;
        Ok(())
    }

    pub fn live_pr(&mut self, slug: &str, number: u64) -> Result<LivePr, ApiError> {
        let path = format!("/repos/{slug}/pulls/{number}");
        let body = self
            .get(&path)?
            .into_body(&path)
            .map_err(|error| self.redact(error))?;
        let text = |pointer: &str| {
            body.pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Ok(LivePr {
            open: text("/state") == "open",
            merged: body.get("merged").and_then(Value::as_bool).unwrap_or(false),
            head_sha: text("/head/sha"),
            base: text("/base/ref"),
            mergeable_state: text("/mergeable_state"),
            merge_commit_sha: body
                .get("merge_commit_sha")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    pub fn merge_pr(
        &mut self,
        slug: &str,
        number: u64,
        sha: &str,
        method: &str,
    ) -> Result<String, ApiError> {
        let path = format!("/repos/{slug}/pulls/{number}/merge");
        let body = json!({ "sha": sha, "merge_method": method });
        let reply = self.put_json(&path, &body)?;
        let merged = reply
            .into_write(&format!("PUT {path}"))
            .map_err(|error| self.redact(error))?;
        match merged.get("sha").and_then(Value::as_str) {
            Some(commit) if merged.get("merged").and_then(Value::as_bool) != Some(false) => {
                Ok(commit.to_string())
            }
            _ => Err(ApiError::Other {
                message: format!("GitHub did not confirm the merge of {slug}#{number}"),
            }),
        }
    }

    pub fn push_runs(&mut self, slug: &str, sha: &str) -> Result<Vec<Run>, ApiError> {
        let path = format!("/repos/{slug}/actions/runs?head_sha={sha}&event=push&per_page=100");
        let body = self
            .get(&path)?
            .into_body(&path)
            .map_err(|error| self.redact(error))?;
        let runs = body
            .get("workflow_runs")
            .and_then(Value::as_array)
            .map(|runs| runs.iter().map(parse_run).collect())
            .unwrap_or_default();
        Ok(runs)
    }

    pub fn tree(&mut self, slug: &str, sha: &str) -> Result<(Vec<String>, bool)> {
        if let Some(found) = self
            .cache
            .get(slug, "tree", &[sha])
            .and_then(|value| cached::tree_from(&value))
        {
            return Ok(found);
        }
        let (paths, truncated) = self.fetch_tree(slug, sha)?;
        let value = cached::tree_value(&paths, truncated);
        self.cache.put(slug, "tree", &[sha], &value);
        Ok((paths, truncated))
    }

    fn fetch_tree(&mut self, slug: &str, sha: &str) -> Result<(Vec<String>, bool)> {
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
        let mut known = cached::blob_map(self.cache.get(slug, "blobs", &[sha]));
        let missing = cached::missing(&known, paths);
        if !missing.is_empty() {
            let found = self.fetch_blobs(slug, sha, &missing)?;
            cached::merge(&mut known, &missing, found);
            self.cache
                .put(slug, "blobs", &[sha], &cached::blobs_value(&known));
        }
        Ok(cached::answer(&known, paths))
    }

    pub fn oids(&mut self, slug: &str, sha: &str) -> BTreeMap<String, Option<String>> {
        if sha.is_empty() {
            return BTreeMap::new();
        }
        cached::oid_map(self.cache.get(slug, "oids", &[sha]))
    }

    pub fn store_oids(&mut self, slug: &str, sha: &str, oids: &BTreeMap<String, Option<String>>) {
        if sha.is_empty() {
            return;
        }
        self.cache
            .put(slug, "oids", &[sha], &cached::oids_value(oids));
    }

    fn fetch_blobs(
        &mut self,
        slug: &str,
        sha: &str,
        paths: &[String],
    ) -> Result<Vec<(String, String)>> {
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
