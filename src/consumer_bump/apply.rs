use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};

use super::backend::{ConsumerRemote, GithubRemote};
use super::{BumpGateway, Consumer, ensure_head, pin_commit_push, safe_path};
use crate::command::bump::{Status, classify};
use crate::feed::Ecosystem;
use crate::github::Client;

const NPM_LOCKS: [&str; 6] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
];

const NPM_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

pub struct Candidate<'a> {
    pub repo: &'a str,
    pub default_branch: &'a str,
    pub head_sha: &'a str,
    pub tree: &'a [String],
    pub rows: &'a [(String, String)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub branch: String,
    pub commit: String,
    pub url: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Opened(Box<Opened>),
    Skipped(String),
}

pub fn branch_name(package: &str, version: &str) -> String {
    let clean = |text: &str| {
        text.to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    };
    format!("pipetrain/bump/{}-{}", clean(package), clean(version))
}

pub fn pull_request_title(package: &str, version: &str) -> String {
    format!("Bump {package} to {version}")
}

pub fn pull_request_body(
    ecosystem: Ecosystem,
    package: &str,
    version: &str,
    rows: &[(String, String)],
) -> String {
    let files = rows
        .iter()
        .map(|(path, current)| format!("- `{path}`: {current} -> {version}"))
        .collect::<Vec<_>>()
        .join("\n");
    let closing = match ecosystem {
        Ecosystem::Nuget => {
            "The commit also holds the restored `packages.lock.json` files. Restore and locked-mode restore passed in an isolated workspace."
        }
        Ecosystem::Npm => {
            "The repository has no npm lock file next to these manifests, so only the manifests changed."
        }
    };
    format!(
        "pipetrain opened this draft pull request with `pipetrain bump --apply`.\n\nIt moves `{package}` to {version}. That version was found in the package feed before this branch was created.\n\n{files}\n\n{closing}\n\nReview it and let CI run before merging; pipetrain does not merge it."
    )
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn sibling(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

pub fn find_solution(tree: &[String]) -> Result<String, String> {
    let found: Vec<&String> = tree
        .iter()
        .filter(|path| path.ends_with(".sln") || path.ends_with(".slnx"))
        .collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err("no solution file in the repository, so there is nothing to restore".into()),
        many => Err(format!(
            "{} solution files in the repository; pipetrain does not guess which one to restore",
            many.len()
        )),
    }
}

pub fn check_nuget(tree: &[String], rows: &[(String, String)]) -> Result<String, String> {
    let solution = find_solution(tree)?;
    let locks: BTreeSet<&str> = tree
        .iter()
        .map(String::as_str)
        .filter(|path| path.ends_with("packages.lock.json"))
        .collect();
    if locks.is_empty() {
        return Err(
            "the repository has no packages.lock.json, so a locked restore cannot prove the pin"
                .into(),
        );
    }
    for (path, _) in rows {
        let project =
            path.ends_with(".csproj") || path.ends_with(".vbproj") || path.ends_with(".fsproj");
        if project && !locks.contains(sibling(directory(path), "packages.lock.json").as_str()) {
            return Err(format!("{path} has no packages.lock.json next to it"));
        }
    }
    Ok(solution)
}

pub fn check_npm(tree: &[String], rows: &[(String, String)]) -> Result<(), String> {
    let present: BTreeSet<&str> = tree.iter().map(String::as_str).collect();
    for (path, _) in rows {
        for dir in [directory(path), ""] {
            for lock in NPM_LOCKS {
                if present.contains(sibling(dir, lock).as_str()) {
                    return Err(format!(
                        "{path} is covered by {} and pipetrain does not update npm lock files",
                        sibling(dir, lock)
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn pin_npm_manifest(text: &str, package: &str, version: &str) -> Result<String> {
    let value: Value = serde_json::from_str(text).context("package.json is not valid JSON")?;
    let mut found: BTreeMap<(String, String), usize> = BTreeMap::new();
    for section in NPM_SECTIONS {
        let Some(map) = value.get(section).and_then(Value::as_object) else {
            continue;
        };
        for (key, spec) in map {
            if key.eq_ignore_ascii_case(package) {
                let spec = spec
                    .as_str()
                    .context("dependency version must be a string")?;
                *found.entry((key.clone(), spec.to_string())).or_default() += 1;
            }
        }
    }
    if found.is_empty() {
        bail!("requested package not found in selected manifest");
    }
    let mut patched = text.to_string();
    for ((key, spec), occurrences) in found {
        if classify(&spec, version) != Status::Change {
            continue;
        }
        let pattern = Regex::new(&format!(
            r#"("{}"\s*:\s*"){}(")"#,
            regex::escape(&key),
            regex::escape(&spec)
        ))?;
        if pattern.find_iter(&patched).count() != occurrences {
            bail!("could not edit {key} in place without touching other entries");
        }
        patched = pattern
            .replace_all(&patched, |found: &regex::Captures| {
                format!("{}{version}{}", &found[1], &found[2])
            })
            .into_owned();
    }
    Ok(patched)
}

fn pin_npm_commit_push(
    package: &str,
    version: &str,
    consumer: &Consumer,
    gateway: &mut impl BumpGateway,
) -> Result<Option<String>> {
    ensure_head(gateway, consumer)?;
    gateway.create_worktree(consumer)?;
    if !gateway.changed_files()?.is_empty() {
        bail!("new bump workspace is not clean");
    }
    let mut patches = BTreeMap::new();
    for path in &consumer.manifests {
        let original = gateway.read(path)?;
        let patched = pin_npm_manifest(&original, package, version)?;
        if patched != original {
            patches.insert(path.clone(), patched);
        }
    }
    if patches.is_empty() {
        return Ok(None);
    }
    for (path, text) in &patches {
        gateway.write(path, text)?;
    }
    let changed = gateway.changed_files()?;
    if changed.iter().collect::<BTreeSet<_>>() != patches.keys().collect::<BTreeSet<_>>() {
        bail!("the workspace changed files other than the selected manifests");
    }
    ensure_head(gateway, consumer)?;
    let commit = gateway.commit(&changed, &format!("bump {package} to {version}"))?;
    ensure_head(gateway, consumer)?;
    gateway.push_new_branch(&consumer.bump_branch, &commit)?;
    ensure_head(gateway, consumer)?;
    Ok(Some(commit))
}

pub fn apply_repo(
    ecosystem: Ecosystem,
    package: &str,
    version: &str,
    candidate: &Candidate,
    gateway: &mut impl BumpGateway,
) -> Result<Applied> {
    if candidate.rows.is_empty() || candidate.rows.iter().any(|(path, _)| !safe_path(path)) {
        bail!("manifest paths must be relative paths inside the repository");
    }
    let solution = match ecosystem {
        Ecosystem::Nuget => match check_nuget(candidate.tree, candidate.rows) {
            Ok(solution) => solution,
            Err(reason) => return Ok(Applied::Skipped(reason)),
        },
        Ecosystem::Npm => match check_npm(candidate.tree, candidate.rows) {
            Ok(()) => String::new(),
            Err(reason) => return Ok(Applied::Skipped(reason)),
        },
    };
    let branch = branch_name(package, version);
    let files: Vec<String> = candidate
        .rows
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    let consumer = Consumer {
        repository: candidate.repo.to_string(),
        number: 0,
        head_branch: candidate.default_branch.to_string(),
        head_sha: candidate.head_sha.to_string(),
        default_branch: candidate.default_branch.to_string(),
        bump_branch: branch.clone(),
        solution,
        manifests: files.clone(),
    };
    let commit = match ecosystem {
        Ecosystem::Nuget => {
            let pins = BTreeMap::from([(
                package.to_ascii_lowercase(),
                (package.to_string(), version.to_string()),
            )]);
            pin_commit_push(&pins, &consumer, gateway)?.map(|pinned| pinned.commit)
        }
        Ecosystem::Npm => pin_npm_commit_push(package, version, &consumer, gateway)?,
    };
    let Some(commit) = commit else {
        return Ok(Applied::Skipped(
            "the manifests already pin that version".into(),
        ));
    };
    let body = pull_request_body(ecosystem, package, version, candidate.rows);
    let url = gateway.open_review_pr(&consumer, &commit, &body)?;
    Ok(Applied::Opened(Box::new(Opened {
        branch,
        commit,
        url,
        files,
    })))
}

pub struct DefaultBranchRemote<'a> {
    pub client: &'a mut Client,
    pub title: String,
}

impl ConsumerRemote for DefaultBranchRemote<'_> {
    fn head(&mut self, consumer: &Consumer) -> Result<String> {
        self.client
            .branch_tip(&consumer.repository, &consumer.default_branch)?
            .context("the default branch no longer exists")
    }

    fn reserve_branch(&mut self, repository: &str, branch: &str, source_sha: &str) -> Result<()> {
        GithubRemote {
            client: &mut *self.client,
        }
        .reserve_branch(repository, branch, source_sha)
    }

    fn open(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String> {
        if self
            .client
            .branch_tip(&consumer.repository, &consumer.bump_branch)?
            .as_deref()
            != Some(commit)
        {
            bail!("remote bump branch no longer matches the prepared commit");
        }
        let reply = self.client.post_json(
            &format!("/repos/{}/pulls", consumer.repository),
            &json!({
                "title": self.title, "body": body,
                "head": consumer.bump_branch, "base": consumer.default_branch, "draft": true,
            }),
        )?;
        if reply.status != 201 {
            bail!("GitHub did not create the bump pull request");
        }
        Ok(reply.body["html_url"]
            .as_str()
            .context("missing bump PR URL")?
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn rows(paths: &[&str]) -> Vec<(String, String)> {
        paths
            .iter()
            .map(|p| (p.to_string(), "1.0.0".to_string()))
            .collect()
    }

    #[test]
    fn the_branch_name_is_isolated_and_safe_for_scoped_packages() {
        assert_eq!(
            branch_name("@acme/ui", "2.0.0"),
            "pipetrain/bump/acme-ui-2.0.0"
        );
        assert_eq!(
            branch_name("Acme.Util", "2.0.0-beta.1+7"),
            "pipetrain/bump/acme.util-2.0.0-beta.1-7"
        );
    }

    #[test]
    fn nuget_needs_one_solution_and_lock_files_next_to_projects() {
        let ok = tree(&[
            "Acme.sln",
            "src/Api/Api.csproj",
            "src/Api/packages.lock.json",
        ]);
        assert_eq!(
            check_nuget(&ok, &rows(&["src/Api/Api.csproj"])),
            Ok("Acme.sln".to_string())
        );
        let two = tree(&["A.sln", "B.sln", "Api.csproj", "packages.lock.json"]);
        assert!(
            check_nuget(&two, &rows(&["Api.csproj"]))
                .unwrap_err()
                .contains("2 solution files")
        );
        let none = tree(&["Api.csproj", "packages.lock.json"]);
        assert!(
            check_nuget(&none, &rows(&["Api.csproj"]))
                .unwrap_err()
                .contains("no solution file")
        );
        let unlocked = tree(&["Acme.sln", "Api.csproj"]);
        assert!(
            check_nuget(&unlocked, &rows(&["Api.csproj"]))
                .unwrap_err()
                .contains("no packages.lock.json")
        );
        let partial = tree(&[
            "Acme.sln",
            "a/A.csproj",
            "b/B.csproj",
            "a/packages.lock.json",
        ]);
        assert_eq!(
            check_nuget(&partial, &rows(&["b/B.csproj"])).unwrap_err(),
            "b/B.csproj has no packages.lock.json next to it"
        );
    }

    #[test]
    fn central_props_need_only_some_lock_file() {
        let found = tree(&[
            "Acme.slnx",
            "Directory.Packages.props",
            "x/packages.lock.json",
        ]);
        assert_eq!(
            check_nuget(&found, &rows(&["Directory.Packages.props"])),
            Ok("Acme.slnx".to_string())
        );
    }

    #[test]
    fn npm_stops_at_any_lock_file_beside_the_manifest_or_at_the_root() {
        assert!(check_npm(&tree(&["web/package.json"]), &rows(&["web/package.json"])).is_ok());
        let beside = tree(&["web/package.json", "web/yarn.lock"]);
        assert!(
            check_npm(&beside, &rows(&["web/package.json"]))
                .unwrap_err()
                .contains("web/yarn.lock")
        );
        let root = tree(&["pnpm-lock.yaml", "web/package.json"]);
        assert!(
            check_npm(&root, &rows(&["web/package.json"]))
                .unwrap_err()
                .contains("pnpm-lock.yaml")
        );
    }

    #[test]
    fn the_npm_edit_changes_only_the_exact_older_version_and_keeps_the_layout() {
        let text = "{\n  \"name\": \"web\",\n  \"dependencies\": {\n    \"@acme/ui\": \"1.0.0\",\n    \"other\": \"1.0.0\"\n  }\n}\n";
        let patched = pin_npm_manifest(text, "@acme/ui", "2.0.0").unwrap();
        assert_eq!(
            patched,
            text.replace("\"@acme/ui\": \"1.0.0\"", "\"@acme/ui\": \"2.0.0\"")
        );
    }

    #[test]
    fn the_npm_edit_leaves_ranges_newer_versions_and_current_ones_alone() {
        for spec in ["^1.0.0", "3.0.0", "2.0.0"] {
            let text = format!("{{\"dependencies\":{{\"@acme/ui\":\"{spec}\"}}}}");
            assert_eq!(pin_npm_manifest(&text, "@acme/ui", "2.0.0").unwrap(), text);
        }
    }

    #[test]
    fn the_npm_edit_refuses_a_package_that_is_not_there_or_an_ambiguous_match() {
        let missing = "{\"dependencies\":{\"other\":\"1.0.0\"}}";
        assert!(pin_npm_manifest(missing, "@acme/ui", "2.0.0").is_err());
        let overrides =
            "{\"dependencies\":{\"@acme/ui\":\"1.0.0\"},\"overrides\":{\"@acme/ui\":\"1.0.0\"}}";
        assert!(pin_npm_manifest(overrides, "@acme/ui", "2.0.0").is_err());
        assert!(pin_npm_manifest("not json", "@acme/ui", "2.0.0").is_err());
    }

    #[test]
    fn the_body_names_the_package_the_versions_and_pipetrain() {
        let body = pull_request_body(
            Ecosystem::Npm,
            "@acme/ui",
            "2.0.0",
            &[("web/package.json".to_string(), "1.0.0".to_string())],
        );
        assert!(body.contains("pipetrain opened this draft pull request"));
        assert!(body.contains("`@acme/ui` to 2.0.0"));
        assert!(body.contains("`web/package.json`: 1.0.0 -> 2.0.0"));
        assert_eq!(
            pull_request_title("@acme/ui", "2.0.0"),
            "Bump @acme/ui to 2.0.0"
        );
    }
}
