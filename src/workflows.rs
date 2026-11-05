use anyhow::{Result, bail};
use regex::Regex;
use yaml_rust2::{Yaml, YamlLoader};

use crate::github::Client;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushRuns {
    No,
    Yes(String),
}

fn is_workflow_path(path: &str) -> bool {
    path.strip_prefix(".github/workflows/").is_some_and(|name| {
        !name.contains('/') && (name.ends_with(".yml") || name.ends_with(".yaml"))
    })
}

pub fn read(client: &mut Client, slug: &str, sha: &str) -> Result<Vec<(String, String)>> {
    let (paths, truncated) = client.tree(slug, sha)?;
    if truncated {
        bail!("the file tree of {slug} is too large to list");
    }
    let files: Vec<String> = paths
        .into_iter()
        .filter(|path| is_workflow_path(path))
        .collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let found = client.blobs(slug, sha, &files)?;
    if found.len() != files.len() {
        bail!("not every workflow file of {slug} could be read");
    }
    Ok(found)
}

fn key_is(key: &Yaml, name: &str) -> bool {
    matches!(key, Yaml::String(text) if text == name)
}

fn lookup<'a>(map: &'a yaml_rust2::yaml::Hash, name: &str) -> Option<&'a Yaml> {
    map.iter()
        .find(|(key, _)| key_is(key, name))
        .map(|(_, value)| value)
}

fn patterns(value: &Yaml) -> Vec<String> {
    match value {
        Yaml::String(text) => vec![text.clone()],
        Yaml::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Yaml::String(text) => Some(text.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn glob(pattern: &str, branch: &str) -> Result<bool, String> {
    if pattern.contains(['?', '+', '[', ']', '\\']) {
        return Err(format!("the filter `{pattern}` is not one it can read"));
    }
    let mut source = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                source.push_str(".*");
            } else {
                source.push_str("[^/]*");
            }
        } else {
            source.push_str(&regex::escape(&c.to_string()));
        }
    }
    source.push('$');
    Regex::new(&source)
        .map(|regex| regex.is_match(branch))
        .map_err(|error| error.to_string())
}

fn selected(list: &[String], branch: &str) -> Result<bool, String> {
    let mut matched = false;
    for pattern in list {
        match pattern.strip_prefix('!') {
            Some(negated) => {
                if glob(negated, branch)? {
                    matched = false;
                }
            }
            None => {
                if glob(pattern, branch)? {
                    matched = true;
                }
            }
        }
    }
    Ok(matched)
}

fn push_filters(map: &yaml_rust2::yaml::Hash, branch: &str) -> Result<bool, String> {
    if let Some(branches) = lookup(map, "branches") {
        return selected(&patterns(branches), branch);
    }
    if let Some(ignored) = lookup(map, "branches-ignore") {
        return Ok(!selected(&patterns(ignored), branch)?);
    }
    let only_tags = lookup(map, "tags").is_some() || lookup(map, "tags-ignore").is_some();
    Ok(!only_tags)
}

fn runs_on_push(on: &Yaml, branch: &str) -> Result<bool, String> {
    match on {
        Yaml::String(event) => Ok(event == "push"),
        Yaml::Array(events) => Ok(events.iter().any(|event| key_is(event, "push"))),
        Yaml::Hash(map) => match lookup(map, "push") {
            None => Ok(false),
            Some(Yaml::Hash(filters)) => push_filters(filters, branch),
            Some(_) => Ok(true),
        },
        _ => Err("the `on` block is not one it can read".to_string()),
    }
}

fn on_block(document: &Yaml) -> Option<&Yaml> {
    let map = document.as_hash()?;
    map.iter()
        .find(|(key, _)| key_is(key, "on") || matches!(key, Yaml::Boolean(true)))
        .map(|(_, value)| value)
}

pub fn push_runs(files: &[(String, String)], branch: &str) -> Result<PushRuns, String> {
    for (path, text) in files {
        let name = path.rsplit('/').next().unwrap_or(path);
        let documents = YamlLoader::load_from_str(text)
            .map_err(|error| format!("{name} is not valid YAML: {error}"))?;
        let Some(on) = documents.first().and_then(on_block) else {
            continue;
        };
        if runs_on_push(on, branch).map_err(|reason| format!("{name}: {reason}"))? {
            return Ok(PushRuns::Yes(name.to_string()));
        }
    }
    Ok(PushRuns::No)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, text: &str) -> (String, String) {
        (format!(".github/workflows/{name}"), text.to_string())
    }

    fn runs(text: &str, branch: &str) -> Result<PushRuns, String> {
        push_runs(&[file("ci.yml", text)], branch)
    }

    const BRANCH: &str = "deploy/05-11-2026-main";

    #[test]
    fn only_top_level_yaml_files_of_the_workflows_folder_count() {
        assert!(is_workflow_path(".github/workflows/ci.yml"));
        assert!(is_workflow_path(".github/workflows/ci.yaml"));
        assert!(!is_workflow_path(".github/workflows/sub/ci.yml"));
        assert!(!is_workflow_path(".github/workflows/readme.md"));
        assert!(!is_workflow_path(".github/dependabot.yml"));
    }

    #[test]
    fn a_bare_push_runs_on_every_branch() {
        assert_eq!(
            runs("on: push\njobs: {}", BRANCH),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
        assert_eq!(
            runs("on: [pull_request, push]\njobs: {}", BRANCH),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
        assert_eq!(
            runs("on:\n  push:\njobs: {}", BRANCH),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
    }

    #[test]
    fn other_events_do_not_run_on_a_push() {
        assert_eq!(runs("on: pull_request", BRANCH), Ok(PushRuns::No));
        assert_eq!(
            runs("on:\n  pull_request:\n  workflow_dispatch:\n", BRANCH),
            Ok(PushRuns::No)
        );
        assert_eq!(runs("name: x\njobs: {}", BRANCH), Ok(PushRuns::No));
    }

    #[test]
    fn a_branches_filter_decides_with_the_github_globs() {
        let only_main = "on:\n  push:\n    branches: [main]\n";
        assert_eq!(runs(only_main, BRANCH), Ok(PushRuns::No));
        let deploy = "on:\n  push:\n    branches:\n      - 'deploy/**'\n";
        assert_eq!(
            runs(deploy, BRANCH),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
        let one_level = "on:\n  push:\n    branches: ['deploy/*']\n";
        assert_eq!(
            runs(one_level, BRANCH),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
        assert_eq!(runs(one_level, "deploy/a/b"), Ok(PushRuns::No));
    }

    #[test]
    fn a_negated_pattern_takes_a_branch_back_out() {
        let text =
            "on:\n  push:\n    branches:\n      - 'deploy/**'\n      - '!deploy/05-11-2026-main'\n";
        assert_eq!(runs(text, BRANCH), Ok(PushRuns::No));
        assert_eq!(
            runs(text, "deploy/06-11-2026-main"),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
    }

    #[test]
    fn branches_ignore_and_tags_only_filters() {
        let ignore = "on:\n  push:\n    branches-ignore: ['deploy/**']\n";
        assert_eq!(runs(ignore, BRANCH), Ok(PushRuns::No));
        assert_eq!(
            runs(ignore, "main"),
            Ok(PushRuns::Yes("ci.yml".to_string()))
        );
        let tags = "on:\n  push:\n    tags: ['v*']\n";
        assert_eq!(runs(tags, BRANCH), Ok(PushRuns::No));
    }

    #[test]
    fn a_paths_filter_is_treated_as_a_possible_run() {
        let text = "on:\n  push:\n    paths: ['src/**']\n";
        assert_eq!(runs(text, BRANCH), Ok(PushRuns::Yes("ci.yml".to_string())));
    }

    #[test]
    fn what_it_cannot_read_is_an_error_never_a_no() {
        assert!(runs("on: [", BRANCH).is_err());
        assert!(runs("on:\n  push:\n    branches: ['release/v?']\n", BRANCH).is_err());
        assert!(runs("on: 5", BRANCH).is_err());
    }

    #[test]
    fn the_first_workflow_that_runs_names_the_answer() {
        let files = vec![
            file("lint.yml", "on: pull_request"),
            file("deploy.yml", "on:\n  push:\n    branches: ['deploy/**']\n"),
        ];
        assert_eq!(
            push_runs(&files, BRANCH),
            Ok(PushRuns::Yes("deploy.yml".to_string()))
        );
        assert_eq!(push_runs(&[], BRANCH), Ok(PushRuns::No));
    }
}
