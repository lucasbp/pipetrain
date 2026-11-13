use anyhow::{Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use yaml_rust2::{Yaml, YamlLoader};

use crate::github::Client;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PushRuns {
    Expected {
        workflows: Vec<String>,
    },
    Dispensed {
        workflows: Vec<Exclusion>,
        reason: String,
    },
    Indeterminate {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusion {
    pub workflow: String,
    pub filter: String,
    pub patterns: Vec<String>,
    pub files: Vec<String>,
}

impl PushRuns {
    pub fn indeterminate(reason: impl Into<String>) -> Self {
        Self::Indeterminate {
            reason: reason.into(),
        }
    }

    pub fn detail(&self, branch: &str) -> String {
        match self {
            Self::Expected { workflows } => format!(
                "expected: workflow {} runs on push to {branch}",
                workflows.join(", ")
            ),
            Self::Dispensed { workflows, reason } => {
                let evidence = workflows
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}: {} {:?} excluded {:?}",
                            entry.workflow, entry.filter, entry.patterns, entry.files
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                if evidence.is_empty() {
                    format!("dispensed: {reason}")
                } else {
                    format!("dispensed: {reason}; {evidence}")
                }
            }
            Self::Indeterminate { reason } => {
                format!("indeterminate: could not read the workflows or push inputs ({reason})")
            }
        }
    }
}

fn is_workflow_path(path: &str) -> bool {
    path.strip_prefix(".github/workflows/").is_some_and(|name| {
        !name.contains('/') && (name.ends_with(".yml") || name.ends_with(".yaml"))
    })
}

pub fn read(client: &mut Client, slug: &str, sha: &str) -> Result<Vec<(String, String)>> {
    let tree = client.rest(&format!("/repos/{slug}/git/trees/{sha}?recursive=1"))?;
    let truncated = tree
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("the file tree of {slug} has no truncation status"))?;
    if truncated {
        bail!("the file tree of {slug} is truncated");
    }
    let entries = tree
        .get("tree")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("the file tree of {slug} is unreadable"))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| anyhow::anyhow!("a file tree path is missing"))?;
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("blob") => {
                if is_workflow_path(path) {
                    files.push(path.to_string());
                }
            }
            Some("tree" | "commit") => {}
            _ => bail!("a file tree entry has an unreadable type"),
        }
    }
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

fn patterns(value: &Yaml) -> Result<Vec<String>, String> {
    let list = match value {
        Yaml::String(text) => vec![text.clone()],
        Yaml::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "a filter pattern is not a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("a filter is not a string or list".to_string()),
    };
    if list.is_empty() || !list.iter().any(|pattern| !pattern.starts_with('!')) {
        return Err("a filter needs a positive pattern".to_string());
    }
    Ok(list)
}

fn glob(pattern: &str) -> Result<Regex, String> {
    if pattern.is_empty()
        || pattern.contains(['?', '+', '[', ']', '\\', '{', '}', '!'])
        || pattern.contains("***")
    {
        return Err(format!("unsupported glob `{pattern}`"));
    }
    let mut source = String::from(r"(?s)\A");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    source.push_str("(?:.*/)?");
                } else {
                    source.push_str(".*");
                }
            } else {
                source.push_str("[^/]*");
            }
        } else {
            source.push_str(&regex::escape(&c.to_string()));
        }
    }
    source.push_str(r"\z");
    Regex::new(&source).map_err(|error| error.to_string())
}

struct Filter {
    name: String,
    patterns: Vec<String>,
    globs: Vec<(bool, Regex)>,
    ignore: bool,
}

impl Filter {
    fn read(
        map: &yaml_rust2::yaml::Hash,
        include: &str,
        ignore: &str,
    ) -> Result<Option<Self>, String> {
        if lookup(map, include).is_some() && lookup(map, ignore).is_some() {
            return Err(format!("{include} and {ignore} cannot be combined"));
        }
        let Some((name, value)) = lookup(map, include)
            .map(|value| (include, value))
            .or_else(|| lookup(map, ignore).map(|value| (ignore, value)))
        else {
            return Ok(None);
        };
        let patterns = patterns(value)?;
        let globs = patterns
            .iter()
            .map(|pattern| match pattern.strip_prefix('!') {
                Some(pattern) => glob(pattern).map(|regex| (false, regex)),
                None => glob(pattern).map(|regex| (true, regex)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Self {
            name: name.to_string(),
            patterns,
            globs,
            ignore: name == ignore,
        }))
    }

    fn matches(&self, name: &str) -> bool {
        let mut matched = false;
        for (positive, regex) in &self.globs {
            if regex.is_match(name) {
                matched = *positive;
            }
        }
        matched != self.ignore
    }

    fn exclusion(self, workflow: &str, files: &[String]) -> Exclusion {
        Exclusion {
            workflow: workflow.to_string(),
            filter: self.name,
            patterns: self.patterns,
            files: files.to_vec(),
        }
    }
}

enum Workflow {
    NoPush,
    Expected,
    Excluded(Exclusion),
}

fn workflow(
    text: &str,
    path: &str,
    branch: &str,
    changed: Option<&[String]>,
) -> Result<Workflow, String> {
    let documents =
        YamlLoader::load_from_str(text).map_err(|error| format!("invalid YAML: {error}"))?;
    if documents.len() != 1 {
        return Err("expected one workflow YAML document".to_string());
    }
    let map = documents[0].as_hash().ok_or("workflow is not a mapping")?;
    let on = map
        .iter()
        .find(|(key, _)| key_is(key, "on") || matches!(key, Yaml::Boolean(true)))
        .map(|(_, value)| value)
        .ok_or("missing on block")?;
    let filters = match on {
        Yaml::String(event) => {
            return Ok(if event == "push" {
                Workflow::Expected
            } else {
                Workflow::NoPush
            });
        }
        Yaml::Array(events) => {
            if events.iter().any(|event| event.as_str().is_none()) {
                return Err("an event is not a string".to_string());
            }
            return Ok(if events.iter().any(|event| key_is(event, "push")) {
                Workflow::Expected
            } else {
                Workflow::NoPush
            });
        }
        Yaml::Hash(events) => match lookup(events, "push") {
            None => return Ok(Workflow::NoPush),
            Some(Yaml::Null) => return Ok(Workflow::Expected),
            Some(Yaml::Hash(filters)) => filters,
            Some(_) => return Err("push filters are not a mapping".to_string()),
        },
        _ => return Err("the on block is not readable".to_string()),
    };
    for (key, _) in filters {
        if ![
            "branches",
            "branches-ignore",
            "paths",
            "paths-ignore",
            "tags",
            "tags-ignore",
        ]
        .iter()
        .any(|name| key_is(key, name))
        {
            return Err("unsupported push filter".to_string());
        }
    }
    let branches = Filter::read(filters, "branches", "branches-ignore")?;
    let paths = if changed.is_some() {
        Filter::read(filters, "paths", "paths-ignore")?
    } else {
        None
    };
    let tags = Filter::read(filters, "tags", "tags-ignore")?;
    if let Some(filter) = branches {
        if !filter.matches(branch) {
            return Ok(Workflow::Excluded(
                filter.exclusion(path, changed.unwrap_or_default()),
            ));
        }
    } else if let Some(filter) = tags {
        return Ok(Workflow::Excluded(
            filter.exclusion(path, changed.unwrap_or_default()),
        ));
    }
    if let Some(filter) = paths
        && let Some(changed) = changed
        && !changed.iter().any(|name| filter.matches(name))
    {
        return Ok(Workflow::Excluded(filter.exclusion(path, changed)));
    }
    Ok(Workflow::Expected)
}

pub fn push_runs(files: &[(String, String)], branch: &str, changed: &[String]) -> PushRuns {
    evaluate(files, branch, Some(changed))
}

pub fn branch_push_runs(files: &[(String, String)], branch: &str) -> PushRuns {
    evaluate(files, branch, None)
}

fn evaluate(files: &[(String, String)], branch: &str, changed: Option<&[String]>) -> PushRuns {
    let mut expected = Vec::new();
    let mut excluded = Vec::new();
    for (path, text) in files {
        match workflow(text, path, branch, changed) {
            Ok(Workflow::NoPush) => {}
            Ok(Workflow::Expected) => expected.push(path.clone()),
            Ok(Workflow::Excluded(evidence)) => excluded.push(evidence),
            Err(reason) => return PushRuns::indeterminate(format!("{path}: {reason}")),
        }
    }
    if expected.is_empty() {
        PushRuns::Dispensed {
            reason: if excluded.is_empty() {
                "no workflow in the repo runs on push".to_string()
            } else {
                format!("workflows read, none runs on push to {branch} for these files")
            },
            workflows: excluded,
        }
    } else {
        PushRuns::Expected {
            workflows: expected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(text: &str, branch: &str, changed: &[&str]) -> PushRuns {
        push_runs(
            &[(".github/workflows/ci.yml".into(), text.into())],
            branch,
            &changed
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
    }

    fn expected(value: PushRuns) -> bool {
        matches!(value, PushRuns::Expected { .. })
    }

    #[test]
    fn only_top_level_yaml_files_of_the_workflows_folder_count() {
        assert!(is_workflow_path(".github/workflows/ci.yml"));
        assert!(is_workflow_path(".github/workflows/ci.yaml"));
        assert!(!is_workflow_path(".github/workflows/sub/ci.yml"));
        assert!(!is_workflow_path(".github/workflows/readme.md"));
    }

    #[test]
    fn push_forms_and_other_events() {
        for text in ["on: push", "on: [pull_request, push]", "on:\n  push:"] {
            assert!(expected(runs(text, "main", &["src/app.rs"])));
        }
        assert!(
            matches!(runs("on: pull_request", "main", &[]), PushRuns::Dispensed { workflows, .. } if workflows.is_empty())
        );
    }

    #[test]
    fn branch_and_path_must_both_match() {
        let text = "on:\n  push:\n    branches: main\n    paths: 'src/**'";
        assert!(expected(runs(text, "main", &["src/app.rs"])));
        assert!(!expected(runs(text, "feature", &["src/app.rs"])));
        assert!(
            matches!(runs(text, "main", &["docs/guide.md"]), PushRuns::Dispensed { workflows, .. } if workflows[0].filter == "paths")
        );
    }

    #[test]
    fn ignores_only_exclude_when_every_path_is_ignored() {
        let text = "on:\n  push:\n    branches-ignore: 'deploy/**'\n    paths-ignore: ['docs/**']";
        assert!(!expected(runs(text, "main", &["docs/a.md", "docs/b.md"])));
        assert!(expected(runs(text, "main", &["docs/a.md", "src/app.rs"])));
        assert!(!expected(runs(text, "deploy/a/b", &["src/app.rs"])));
    }

    #[test]
    fn ordered_negations_can_exclude_and_reinclude() {
        let text =
            "on:\n  push:\n    paths: ['src/**', '!src/private/**', 'src/private/public.rs']";
        assert!(!expected(runs(text, "main", &["src/private/key.rs"])));
        assert!(expected(runs(text, "main", &["src/private/public.rs"])));
        let text = "on:\n  push:\n    branches: ['deploy/**', '!deploy/private/**', 'deploy/private/public']";
        assert!(!expected(runs(text, "deploy/private/a", &["a"])));
        assert!(expected(runs(text, "deploy/private/public", &["a"])));
    }

    #[test]
    fn stars_match_slashes_and_root_files_as_github_patterns_do() {
        let one = glob("src/*").unwrap();
        assert!(one.is_match("src/app.rs"));
        assert!(!one.is_match("src/nested/app.rs"));
        let recursive = glob("**/*.rs").unwrap();
        assert!(recursive.is_match("app.rs"));
        assert!(recursive.is_match("src/nested/app.rs"));
        assert!(recursive.is_match("src/new\nline/app.rs"));
        assert!(!recursive.is_match("app.rst"));
    }

    #[test]
    fn unreadable_or_unsupported_inputs_are_never_dispensed() {
        for text in [
            "on: [",
            "on: 5",
            "jobs: {}",
            "on:\n  push:\n    paths: ['x', 5]",
            "on:\n  push:\n    paths: ['x?']",
            "on:\n  push:\n    paths: []",
            "on:\n  push:\n    branches: main\n    branches-ignore: other",
            "on:\n  push:\n    paths: ['!docs/**']",
        ] {
            assert!(
                matches!(runs(text, "main", &["a"]), PushRuns::Indeterminate { .. }),
                "{text}"
            );
        }
    }

    #[test]
    fn all_expected_workflows_are_retained_and_unknowns_win() {
        let mut files = vec![
            (".github/workflows/a.yml".into(), "on: push".into()),
            (".github/workflows/b.yml".into(), "on: push".into()),
        ];
        assert!(
            matches!(push_runs(&files, "main", &[]), PushRuns::Expected { workflows } if workflows.len() == 2)
        );
        files.push((".github/workflows/c.yml".into(), "on: [".into()));
        assert!(matches!(
            push_runs(&files, "main", &[]),
            PushRuns::Indeterminate { .. }
        ));
    }

    #[test]
    fn tags_only_do_not_run_on_branch_pushes() {
        assert!(matches!(
            runs("on:\n  push:\n    tags: 'v*'", "main", &["a"]),
            PushRuns::Dispensed { .. }
        ));
    }
}
