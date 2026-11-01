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

const PR_FIELDS: &str = "number title url body headRefName baseRefName baseRef { target { oid } } headRefOid isDraft state reviewDecision mergeable repository { name owner { login } } latestOpinionatedReviews(first: 20, writersOnly: true) { nodes { state } } commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }";

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
