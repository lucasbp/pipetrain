use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::apply::{Gate, gate, tint};
use super::bump::{BumpOptions, Outcome, Report, Status, restore_note};
use super::feed::{self as feed_command, FeedOptions};
use crate::config::ApiConfig;
use crate::consumer_bump::RestoreSettings;
use crate::consumer_bump::apply::{
    Applied, Candidate, DefaultBranchRemote, apply_repo, pull_request_title, standalone_branch,
};
use crate::consumer_bump::backend::{LocalBackend, ProcessRunner};
use crate::feed::{self, Ecosystem};
use crate::github::{self, Client};
use crate::style::{bold, dim, green, red, yellow};

const WORKSPACE_TIMEOUT: Duration = Duration::from_secs(600);
const STOPPED: &str = "stopped after an earlier failure";

#[derive(Debug, Serialize)]
pub struct RepoResult {
    pub repo: String,
    pub status: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_mode: Option<&'static str>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub pull_request_url: Option<String>,
    pub files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ApplyReport {
    pub schema: u32,
    pub applied: bool,
    pub package: String,
    pub ecosystem: &'static str,
    pub target: String,
    pub repos: Vec<RepoResult>,
}

type Targets = Vec<(String, Vec<(String, String)>)>;

pub fn check_gate(opts: &BumpOptions) -> Result<()> {
    if gate(true, opts.yes, std::io::stdin().is_terminal(), None) == Gate::Refuse {
        bail!("refusing to write without a terminal: pass --yes");
    }
    Ok(())
}

pub fn targets(report: &Report) -> Targets {
    report
        .repos
        .iter()
        .filter_map(|repo| {
            let rows: Vec<(String, String)> = repo
                .files
                .iter()
                .filter(|row| row.status == Status::Change)
                .map(|row| (row.path.clone(), row.current.clone()))
                .collect();
            (!rows.is_empty()).then(|| (repo.repo.clone(), rows))
        })
        .collect()
}

pub fn plan_text(report: &Report, targets: &Targets, color: bool) -> String {
    let count = targets.len();
    let mut out = format!(
        "{}\n",
        tint(
            color,
            bold,
            &format!(
                "open {count} draft PR{} bumping {} to {}",
                if count == 1 { "" } else { "s" },
                report.package,
                report.target
            )
        )
    );
    for (repo, rows) in targets {
        let note = report
            .repos
            .iter()
            .find(|found| found.repo == *repo)
            .and_then(|found| found.restore.as_ref())
            .map(|settings| format!("  {}", tint(color, dim, &restore_note(settings))))
            .unwrap_or_default();
        out.push_str(&format!("{}{note}\n", tint(color, bold, repo)));
        for (path, current) in rows {
            out.push_str(&format!("  {path}  {current} -> {}\n", report.target));
        }
    }
    out
}

fn ask(text: &str, count: usize) -> String {
    eprintln!("{text}");
    eprint!(
        "Push {count} new branch{} and open {count} draft PR{} on GitHub? Type yes: ",
        if count == 1 { "" } else { "es" },
        if count == 1 { "" } else { "s" }
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line
}

fn result(repo: &str, status: &'static str, detail: impl Into<String>) -> RepoResult {
    RepoResult {
        repo: repo.to_string(),
        status,
        detail: detail.into(),
        restore_mode: None,
        branch: None,
        commit: None,
        pull_request_url: None,
        files: Vec::new(),
    }
}

fn cleanup(workspace: Option<PathBuf>) {
    if let Some(path) = workspace
        && path.join("pipetrain-owned").is_file()
    {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn verify_head(ecosystem: Ecosystem, expected: Option<&str>, current: &str) -> Result<()> {
    if matches!(
        ecosystem,
        Ecosystem::Cargo | Ecosystem::Pypi | Ecosystem::Go
    ) && expected != Some(current)
    {
        bail!("the default branch is no longer at the previewed commit; run bump again");
    }
    Ok(())
}

fn open_one(
    client: &mut Client,
    ecosystem: Ecosystem,
    (package, version): (&str, &str),
    (repo, rows): (&str, &[(String, String)]),
    root: &Path,
    settings: &RestoreSettings,
    expected: Option<&str>,
) -> Result<RepoResult> {
    let branch = standalone_branch(ecosystem, package, version);
    let meta = client.rest(&format!("/repos/{repo}"))?;
    let default_branch = meta["default_branch"]
        .as_str()
        .context("GitHub did not name the default branch")?
        .to_string();
    let head = client
        .branch_tip(repo, &default_branch)?
        .context("the default branch was not found")?;
    verify_head(ecosystem, expected, &head)?;
    if client.branch_tip(repo, &branch)?.is_some() {
        return Ok(result(
            repo,
            "skipped",
            format!(
                "branch {branch} already exists, so an earlier run or someone else got there first"
            ),
        ));
    }
    let (tree, truncated) = client.tree(repo, &head)?;
    if truncated {
        return Ok(result(
            repo,
            "skipped",
            "the file tree is too large to inspect",
        ));
    }
    let candidate = Candidate {
        repo,
        default_branch: &default_branch,
        head_sha: &head,
        tree: &tree,
        rows,
    };
    let remote = DefaultBranchRemote {
        client,
        title: pull_request_title(package, version),
    };
    let mut backend =
        LocalBackend::new(root.to_path_buf(), ProcessRunner, remote, WORKSPACE_TIMEOUT);
    let applied = apply_repo(
        ecosystem,
        package,
        version,
        &candidate,
        settings,
        &mut backend,
    );
    let workspace = backend.workspace().map(Path::to_path_buf);
    match applied {
        Ok(Applied::Opened(opened)) => {
            cleanup(workspace);
            Ok(RepoResult {
                branch: Some(opened.branch),
                commit: Some(opened.commit),
                pull_request_url: Some(opened.url),
                files: opened.files,
                ..result(repo, "opened", "draft PR opened")
            })
        }
        Ok(Applied::Skipped(reason)) => {
            cleanup(workspace);
            Ok(result(repo, "skipped", reason))
        }
        Err(error) => {
            let kept = workspace
                .map(|path| format!("; workspace kept at {}", path.display()))
                .unwrap_or_default();
            Ok(result(repo, "failed", format!("{error:#}{kept}")))
        }
    }
}

pub fn apply_all(
    client: &mut Client,
    ecosystem: Ecosystem,
    (package, version): (&str, &str),
    targets: &Targets,
    root: &Path,
    settings: &BTreeMap<String, RestoreSettings>,
    observe: &mut dyn FnMut(&RepoResult),
) -> Vec<RepoResult> {
    apply_confirmed(
        client,
        ecosystem,
        (package, version),
        targets,
        ApplyContext {
            root,
            settings,
            heads: &BTreeMap::new(),
        },
        observe,
    )
}

struct ApplyContext<'a> {
    root: &'a Path,
    settings: &'a BTreeMap<String, RestoreSettings>,
    heads: &'a BTreeMap<String, String>,
}

fn apply_confirmed(
    client: &mut Client,
    ecosystem: Ecosystem,
    (package, version): (&str, &str),
    targets: &Targets,
    context: ApplyContext<'_>,
    observe: &mut dyn FnMut(&RepoResult),
) -> Vec<RepoResult> {
    let ApplyContext {
        root,
        settings,
        heads,
    } = context;
    let mut done = Vec::new();
    let mut stopped = false;
    for (repo, rows) in targets {
        let found = if stopped {
            result(repo, "skipped", STOPPED)
        } else {
            let chosen = settings.get(repo).cloned().unwrap_or_default();
            match open_one(
                client,
                ecosystem,
                (package, version),
                (repo, rows),
                root,
                &chosen,
                heads.get(repo).map(String::as_str),
            ) {
                Ok(found) => found,
                Err(error) => result(repo, "failed", format!("{error:#}")),
            }
        };
        let found = RepoResult {
            restore_mode: (ecosystem == Ecosystem::Nuget)
                .then(|| settings.get(repo).map_or("locked", |s| s.mode.as_str())),
            ..found
        };
        stopped |= found.status == "failed";
        observe(&found);
        done.push(found);
    }
    done
}

fn result_line(found: &RepoResult, color: bool) -> String {
    let label = match found.status {
        "opened" => tint(color, green, "opened"),
        "failed" => tint(color, red, "failed"),
        other => tint(color, dim, other),
    };
    let tail = found.pull_request_url.as_deref().unwrap_or(&found.detail);
    let mode = found
        .restore_mode
        .map(|mode| format!(" ({mode})"))
        .unwrap_or_default();
    format!("{label} {}{mode}  {tail}", found.repo)
}

pub fn summary_line(repos: &[RepoResult], color: bool) -> String {
    let count = |status: &str| repos.iter().filter(|r| r.status == status).count();
    let (opened, failed) = (count("opened"), count("failed"));
    let opened_text = format!("{opened} opened");
    let failed_text = format!("{failed} failed");
    format!(
        "{}, {}, {}",
        if opened > 0 {
            tint(color, green, &opened_text)
        } else {
            opened_text
        },
        tint(color, dim, &format!("{} skipped", count("skipped"))),
        if failed > 0 {
            tint(color, red, &failed_text)
        } else {
            tint(color, dim, &failed_text)
        }
    )
}

fn report_of(report: &Report, applied: bool, repos: Vec<RepoResult>) -> ApplyReport {
    ApplyReport {
        schema: 1,
        applied,
        package: report.package.clone(),
        ecosystem: report.ecosystem.as_str(),
        target: report.target.clone(),
        repos,
    }
}

pub fn run(opts: &BumpOptions, outcome: Outcome) -> Result<i32> {
    let Outcome {
        report,
        before,
        after,
        exit,
        feed,
        api,
        heads,
        ..
    } = outcome;
    for note in &before {
        eprintln!("{note}");
    }
    let (Some(report), Some(feed), Some(api)) = (report, feed, api) else {
        for note in &after {
            eprintln!("{note}");
        }
        return Ok(exit);
    };
    let targets = targets(&report);
    if targets.is_empty() {
        eprintln!("nothing to apply: no manifest would change");
        finish(opts, &report, false, Vec::new())?;
        return Ok(0);
    }
    if let Some(code) = prove(&report, feed)? {
        return Ok(code);
    }
    let text = plan_text(&report, &targets, false);
    if opts.yes {
        eprintln!("{text}");
    } else {
        let answer = ask(&text, targets.len());
        if gate(true, false, std::io::stdin().is_terminal(), Some(&answer)) != Gate::Proceed {
            eprintln!("aborted, nothing was written");
            return Ok(1);
        }
    }
    let repos = write_all(opts, &report, &api, &targets, &heads)?;
    let failed = repos.iter().any(|found| found.status == "failed");
    finish(opts, &report, true, repos)?;
    Ok(i32::from(failed))
}

fn prove(report: &Report, feed: FeedOptions) -> Result<Option<i32>> {
    let options = FeedOptions {
        version: Some(report.target.clone()),
        ..feed
    };
    let mut notes = Vec::new();
    let proven = feed_command::proof(&options, &mut notes)?;
    for note in &notes {
        eprintln!("{note}");
    }
    match proven {
        Err(code) => Ok(Some(code)),
        Ok(false) => {
            eprintln!(
                "{} {} is not in the package feed yet; nothing was written",
                report.package, report.target
            );
            Ok(Some(feed::EXIT_NOT_PUBLISHED))
        }
        Ok(true) => Ok(None),
    }
}

fn write_all(
    opts: &BumpOptions,
    report: &Report,
    api: &ApiConfig,
    targets: &Targets,
    heads: &BTreeMap<String, String>,
) -> Result<Vec<RepoResult>> {
    let root = crate::cache::default_dir()
        .map(|root| root.join("bump-workspaces"))
        .context("no safe cache directory for isolated bump workspaces")?;
    let mut client = Client::new(github::token()?, api, opts.verbose);
    let color = !opts.json;
    let settings: BTreeMap<String, RestoreSettings> = report
        .repos
        .iter()
        .filter_map(|repo| Some((repo.repo.clone(), repo.restore.clone()?)))
        .collect();
    let mut observe = |found: &RepoResult| eprintln!("{}", result_line(found, color));
    Ok(apply_confirmed(
        &mut client,
        report.ecosystem,
        (&report.package, &report.target),
        targets,
        ApplyContext {
            root: &root,
            settings: &settings,
            heads,
        },
        &mut observe,
    ))
}

fn finish(
    opts: &BumpOptions,
    report: &Report,
    applied: bool,
    repos: Vec<RepoResult>,
) -> Result<()> {
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report_of(report, applied, repos))?
        );
    } else if applied {
        anstream::println!("{}", summary_line(&repos, true));
        if repos.iter().any(|found| found.status == "opened") {
            anstream::println!(
                "{}",
                tint(true, yellow, "review the draft PRs and let CI run")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::bump::RepoRefs;

    #[test]
    fn changed_default_head_refuses_manifest_writes_before_reading_the_new_tree() {
        use std::io::BufReader;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (path, body) in [
                ("/repos/acme/demo", r#"{"default_branch":"main"}"#),
                (
                    "/repos/acme/demo/git/ref/heads/main",
                    r#"{"object":{"sha":"bb22"}}"#,
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(&format!("GET {path} ")), "{line}");
                for header in reader.lines() {
                    if header.unwrap().is_empty() {
                        break;
                    }
                }
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let mut client = Client::with_base_url(
            "ghp_FAKE0123456789".into(),
            &ApiConfig::default(),
            false,
            &url,
        );
        let err = open_one(
            &mut client,
            Ecosystem::Cargo,
            ("demo", "1.1.0"),
            ("acme/demo", &[("Cargo.toml".into(), "=1.0.0".into())]),
            Path::new("."),
            &RestoreSettings::default(),
            Some("aa11"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("previewed commit"), "{err}");
        server.join().unwrap();
        assert!(verify_head(Ecosystem::Pypi, None, "bb22").is_err());
        assert!(verify_head(Ecosystem::Go, Some("bb22"), "bb22").is_ok());
        assert!(verify_head(Ecosystem::Nuget, None, "bb22").is_ok());
    }

    fn report() -> Report {
        let repos = vec![
            RepoRefs {
                repo: "acme/web".to_string(),
                refs: vec![
                    ("a/package.json".to_string(), "1.0.0".to_string()),
                    ("b/package.json".to_string(), "2.0.0".to_string()),
                ],
            },
            RepoRefs {
                repo: "acme/docs".to_string(),
                refs: vec![("package.json".to_string(), "^1.0.0".to_string())],
            },
        ];
        Report::build("@acme/ui", Ecosystem::Npm, "2.0.0", &repos)
    }

    #[test]
    fn only_manifests_the_dry_run_lists_as_change_are_targeted() {
        assert_eq!(
            targets(&report()),
            vec![(
                "acme/web".to_string(),
                vec![("a/package.json".to_string(), "1.0.0".to_string())]
            )]
        );
    }

    #[test]
    fn the_plan_names_the_package_the_repos_and_the_versions() {
        let report = report();
        let text = plan_text(&report, &targets(&report), false);
        assert!(text.starts_with("open 1 draft PR bumping @acme/ui to 2.0.0"));
        assert!(text.contains("acme/web"));
        assert!(text.contains("a/package.json  1.0.0 -> 2.0.0"));
    }

    #[test]
    fn the_summary_counts_each_status() {
        let repos = vec![
            result("a", "opened", ""),
            result("b", "skipped", "x"),
            result("c", "failed", "y"),
        ];
        assert_eq!(summary_line(&repos, false), "1 opened, 1 skipped, 1 failed");
    }
}
