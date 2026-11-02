use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde_json::{Value, json};

use super::apply::tint;
use super::feed::{self as feed_command, FeedOptions};
use super::order::{Names, configured, names_of, read_repo, requests_line};
use super::resolve;
use crate::cache::{Cache, CacheMode};
use crate::elapsed;
use crate::feed::{self, Ecosystem};
use crate::github::{self, Client};
use crate::manifest;
use crate::progress::{self, Progress};
use crate::style::{bold, dim, green, yellow};

pub struct BumpOptions {
    pub package: String,
    pub to: Option<String>,
    pub ecosystem: Option<Ecosystem>,
    pub org: Option<String>,
    pub registry: Option<String>,
    pub repos: Vec<String>,
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub verbose: bool,
    pub cache: CacheMode,
    pub progress: bool,
    pub json: bool,
    pub only: Vec<Level>,
    pub changes_only: bool,
    pub check: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Level {
    Major,
    Minor,
    Patch,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Patch => "patch",
        }
    }
}

pub fn level_of(current: &str, target: &str) -> Level {
    let from = feed::Version::parse(current);
    let to = feed::Version::parse(target);
    let (from, to) = (from.numbers(), to.numbers());
    for index in 0..from.len().max(to.len()) {
        if from.get(index).unwrap_or(&0) != to.get(index).unwrap_or(&0) {
            return match index {
                0 => Level::Major,
                1 => Level::Minor,
                _ => Level::Patch,
            };
        }
    }
    Level::Patch
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Change,
    UpToDate,
    Ahead,
    Range,
    OtherLevel,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::UpToDate => "up_to_date",
            Self::Ahead => "ahead",
            Self::Range => "range",
            Self::OtherLevel => "other_level",
        }
    }
}

fn is_exact(spec: &str) -> bool {
    let core = spec.split(['-', '+']).next().unwrap_or_default();
    !core.is_empty()
        && core
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && spec
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

pub fn classify(current: &str, target: &str) -> Status {
    if !is_exact(current) {
        return Status::Range;
    }
    match feed::compare(current, target) {
        std::cmp::Ordering::Equal => Status::UpToDate,
        std::cmp::Ordering::Less => Status::Change,
        std::cmp::Ordering::Greater => Status::Ahead,
    }
}

pub fn target_version(
    to: Option<&str>,
    lookup: impl FnOnce() -> Result<Result<String, i32>>,
) -> Result<Result<String, i32>> {
    match to {
        Some(version) => Ok(Ok(version.trim().to_string())),
        None => lookup(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRefs {
    pub repo: String,
    pub refs: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub repos: Vec<RepoRefs>,
    pub warnings: Vec<String>,
}

fn in_ecosystem(path: &str, ecosystem: Ecosystem) -> bool {
    let lower = path.to_lowercase();
    match ecosystem {
        Ecosystem::Nuget => lower.ends_with("proj") || lower.ends_with(".props"),
        Ecosystem::Npm => lower == "package.json" || lower.ends_with("/package.json"),
    }
}

pub fn refs_of(
    manifests: &[(String, String, String)],
    package: &str,
    ecosystem: Ecosystem,
) -> Vec<(String, String)> {
    manifests
        .iter()
        .filter(|(_, path, _)| in_ecosystem(path, ecosystem))
        .flat_map(|(_, path, text)| {
            manifest::package_versions(path, text, package)
                .into_iter()
                .map(move |spec| (path.clone(), spec))
        })
        .collect()
}

pub fn scan(
    client: &mut Client,
    names: &Names,
    package: &str,
    ecosystem: Ecosystem,
) -> Result<Scan> {
    let mut questions = Vec::new();
    let mut found = Scan::default();
    let mut readable = 0;
    progress::phase("reading manifests");
    for (index, repo) in names.values().enumerate() {
        match read_repo(client, repo, &mut questions)? {
            Some(manifests) => {
                readable += 1;
                let refs = refs_of(&manifests, package, ecosystem);
                if !refs.is_empty() {
                    found.repos.push(RepoRefs {
                        repo: repo.clone(),
                        refs,
                    });
                }
            }
            None => found.warnings.push(format!(
                "warning: skipped {repo}: GitHub answered not found, check the name in the config"
            )),
        }
        progress::count(index + 1, names.len());
    }
    if readable == 0 {
        bail!("no repo could be read: every name was not found, check the names in the config");
    }
    found
        .warnings
        .extend(questions.iter().map(|q| format!("warning: {}", q.message)));
    Ok(found)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub current: String,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRows {
    pub repo: String,
    pub files: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub package: String,
    pub ecosystem: Ecosystem,
    pub target: String,
    pub repos: Vec<RepoRows>,
}

impl Report {
    pub fn build(package: &str, ecosystem: Ecosystem, target: &str, repos: &[RepoRefs]) -> Self {
        let repos = repos
            .iter()
            .map(|found| RepoRows {
                repo: found.repo.clone(),
                files: found
                    .refs
                    .iter()
                    .map(|(path, current)| Row {
                        path: path.clone(),
                        current: current.clone(),
                        status: classify(current, target),
                    })
                    .collect(),
            })
            .collect();
        Self {
            package: package.to_string(),
            ecosystem,
            target: target.to_string(),
            repos,
        }
    }

    pub fn restrict(mut self, only: &[Level]) -> Self {
        if only.is_empty() {
            return self;
        }
        let target = self.target.clone();
        for row in self.repos.iter_mut().flat_map(|repo| repo.files.iter_mut()) {
            if row.status == Status::Change && !only.contains(&level_of(&row.current, &target)) {
                row.status = Status::OtherLevel;
            }
        }
        self
    }

    pub fn count(&self, status: Status) -> usize {
        self.repos
            .iter()
            .flat_map(|repo| &repo.files)
            .filter(|row| row.status == status)
            .count()
    }
}

fn row_text(report: &Report, row: &Row, color: bool) -> String {
    let head = format!("{}  {} {}", row.path, report.package, row.current);
    match row.status {
        Status::Change => format!("{head} -> {}", tint(color, green, &report.target)),
        Status::UpToDate => format!("{head} {}", tint(color, dim, "up to date")),
        Status::Ahead => format!(
            "{head} {}",
            tint(color, dim, "ahead of target (left alone)")
        ),
        Status::Range => format!("{head} {}", tint(color, yellow, "range, left alone")),
        Status::OtherLevel => format!(
            "{head} {}",
            tint(color, dim, "different level (left alone)")
        ),
    }
}

fn summary_line(report: &Report, elapsed: Option<Duration>) -> String {
    let change = report.count(Status::Change);
    let up_to_date = report.count(Status::UpToDate);
    let left = report.count(Status::Ahead)
        + report.count(Status::Range)
        + report.count(Status::OtherLevel);
    let repos = report.repos.len();
    let noun = if repos == 1 { "repo" } else { "repos" };
    let suffix = elapsed
        .map(|took| format!(" in {}", elapsed::format(took)))
        .unwrap_or_default();
    format!(
        "{change} would change, {up_to_date} up to date, {left} left alone in {repos} {noun}{suffix}"
    )
}

pub fn render_with(report: &Report, color: bool, elapsed: Option<Duration>) -> String {
    render_rows(report, color, elapsed, false)
}

pub fn render_rows(
    report: &Report,
    color: bool,
    elapsed: Option<Duration>,
    changes_only: bool,
) -> String {
    let mut out = String::new();
    let title = format!("bump {} to {}", report.package, report.target);
    out.push_str(&format!("{}\n", tint(color, bold, &title)));
    for repo in &report.repos {
        let rows: Vec<&Row> = repo
            .files
            .iter()
            .filter(|row| !changes_only || row.status == Status::Change)
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("{}\n", tint(color, bold, &repo.repo)));
        for row in rows {
            out.push_str(&format!("  {}\n", row_text(report, row, color)));
        }
    }
    out.push_str(&format!("{}\n", summary_line(report, elapsed)));
    out.push_str(&format!(
        "{}\n",
        tint(color, yellow, "dry run: nothing was written")
    ));
    out
}

pub fn render(report: &Report) -> String {
    render_with(report, false, None)
}

pub fn to_json(report: &Report) -> Value {
    to_json_with(report, &[], false)
}

pub fn to_json_with(report: &Report, only: &[Level], changes_only: bool) -> Value {
    let repos: Vec<Value> = report
        .repos
        .iter()
        .filter(|repo| !changes_only || repo.files.iter().any(|row| row.status == Status::Change))
        .map(|repo| {
            let files: Vec<Value> = repo
                .files
                .iter()
                .filter(|row| !changes_only || row.status == Status::Change)
                .map(|row| {
                    json!({
                        "path": row.path,
                        "current": row.current,
                        "status": row.status.as_str(),
                    })
                })
                .collect();
            json!({ "repo": repo.repo, "files": files })
        })
        .collect();
    let mut out = json!({
        "schema": 1,
        "package": report.package,
        "ecosystem": report.ecosystem.as_str(),
        "target": report.target,
        "repos": repos,
        "dry_run": true,
    });
    if !only.is_empty() {
        out["only"] = only.iter().map(|level| level.as_str()).collect();
    }
    out
}

pub fn run(opts: BumpOptions) -> Result<i32> {
    let started = Instant::now();
    let (cfg, _) = resolve::load(opts.config.as_deref(), opts.profile.as_deref(), &[], &[])?;
    let (exact, globs) = configured(&cfg);
    let from_config = opts.repos.is_empty();
    let names = if from_config {
        names_of(exact)
    } else {
        names_of(opts.repos.clone())
    };
    if names.is_empty() {
        bail!("no repos given and the profile has no exact repo names in [[layer]] or [[order]]");
    }
    if from_config && !globs.is_empty() {
        eprintln!("warning: ignored names with a glob: {}", globs.join(", "));
    }
    let ecosystem = opts
        .ecosystem
        .unwrap_or_else(|| Ecosystem::default_for(&opts.package));
    let feed_options = FeedOptions {
        package: opts.package.clone(),
        version: None,
        ecosystem: Some(ecosystem),
        org: opts.org.clone(),
        registry: opts.registry.clone(),
        json: false,
    };
    let target = match target_version(opts.to.as_deref(), || feed_command::latest(&feed_options))? {
        Ok(target) => target,
        Err(code) => return Ok(code),
    };
    if target.is_empty() {
        bail!("--to needs a version");
    }
    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose)
        .with_cache(Cache::at_default(opts.cache));
    let progress = Progress::start(opts.progress);
    let scanned = scan(&mut client, &names, &opts.package, ecosystem)?;
    drop(progress);
    for warning in &scanned.warnings {
        eprintln!("{warning}");
    }
    if scanned.repos.is_empty() {
        eprintln!("{} is not referenced in any scanned manifest", opts.package);
        return Ok(1);
    }
    let report =
        Report::build(&opts.package, ecosystem, &target, &scanned.repos).restrict(&opts.only);
    if opts.json {
        let json = to_json_with(&report, &opts.only, opts.changes_only);
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        anstream::print!(
            "{}",
            render_rows(&report, true, Some(started.elapsed()), opts.changes_only)
        );
    }
    eprintln!(
        "{}",
        requests_line(client.requests(), client.cache_hits(), started.elapsed())
    );
    let pending = report.count(Status::Change);
    if opts.check && pending > 0 {
        eprintln!("--check: {pending} would change");
        return Ok(1);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(path: &str, text: &str) -> (String, String, String) {
        ("acme/api".to_string(), path.to_string(), text.to_string())
    }

    fn csproj(version: &str) -> String {
        format!(
            r#"<Project><PackageReference Include="Acme.Util" Version="{version}" /></Project>"#
        )
    }

    fn sample() -> Report {
        let repos = vec![
            RepoRefs {
                repo: "acme/api".to_string(),
                refs: vec![
                    ("src/A.csproj".to_string(), "1.2.0".to_string()),
                    ("src/B.csproj".to_string(), "1.5.0".to_string()),
                ],
            },
            RepoRefs {
                repo: "acme/web".to_string(),
                refs: vec![
                    ("C.csproj".to_string(), "2.0.0".to_string()),
                    ("D.csproj".to_string(), "[1.0,2.0)".to_string()),
                ],
            },
        ];
        Report::build("Acme.Util", Ecosystem::Nuget, "1.5.0", &repos)
    }

    #[test]
    fn extraction_reads_only_manifests_of_the_chosen_ecosystem() {
        let manifests = vec![
            manifest("src/A.csproj", &csproj("1.2.0")),
            manifest(
                "web/package.json",
                r#"{"dependencies":{"Acme.Util":"1.0.0"}}"#,
            ),
            manifest("Directory.Packages.props", &csproj("1.4.0")),
        ];
        assert_eq!(
            refs_of(&manifests, "Acme.Util", Ecosystem::Nuget),
            vec![
                ("src/A.csproj".to_string(), "1.2.0".to_string()),
                ("Directory.Packages.props".to_string(), "1.4.0".to_string())
            ]
        );
        assert_eq!(
            refs_of(&manifests, "Acme.Util", Ecosystem::Npm),
            vec![("web/package.json".to_string(), "1.0.0".to_string())]
        );
    }

    #[test]
    fn extraction_skips_other_packages() {
        let manifests = vec![manifest("A.csproj", &csproj("1.2.0"))];
        assert!(refs_of(&manifests, "Acme.Other", Ecosystem::Nuget).is_empty());
    }

    #[test]
    fn status_compares_exact_versions_with_the_feed_ordering() {
        assert_eq!(classify("1.2.0", "1.10.0"), Status::Change);
        assert_eq!(classify("1.10.0", "1.10.0"), Status::UpToDate);
        assert_eq!(classify("1.10", "1.10.0"), Status::UpToDate);
        assert_eq!(classify("2.0.0", "1.10.0"), Status::Ahead);
        assert_eq!(classify("1.0.0-beta.1", "1.0.0"), Status::Change);
    }

    #[test]
    fn floating_specs_are_ranges() {
        for spec in [
            "^1.2.0",
            "~1.2.0",
            "*",
            "1.*",
            "[1.0,2.0)",
            ">=1.0.0",
            "latest",
            "1.x",
        ] {
            assert_eq!(classify(spec, "9.9.9"), Status::Range, "{spec}");
        }
    }

    #[test]
    fn the_target_comes_from_to_without_asking_the_feed() {
        let target = target_version(Some(" 1.5.0 "), || panic!("the feed must not be asked"));
        assert_eq!(target.unwrap().unwrap(), "1.5.0");
    }

    #[test]
    fn the_target_comes_from_the_feed_without_to() {
        let target = target_version(None, || Ok(Ok("3.0.0".to_string())));
        assert_eq!(target.unwrap().unwrap(), "3.0.0");
        let refused = target_version(None, || Ok(Err(2)));
        assert_eq!(refused.unwrap().unwrap_err(), 2);
    }

    #[test]
    fn the_plain_render_lists_every_row_and_the_summary() {
        let text = render(&sample());
        let expected = [
            "bump Acme.Util to 1.5.0",
            "acme/api",
            "  src/A.csproj  Acme.Util 1.2.0 -> 1.5.0",
            "  src/B.csproj  Acme.Util 1.5.0 up to date",
            "acme/web",
            "  C.csproj  Acme.Util 2.0.0 ahead of target (left alone)",
            "  D.csproj  Acme.Util [1.0,2.0) range, left alone",
            "1 would change, 1 up to date, 2 left alone in 2 repos",
            "dry run: nothing was written",
            "",
        ]
        .join("\n");
        assert_eq!(text, expected);
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn the_summary_shows_the_elapsed_time() {
        let text = render_with(&sample(), false, Some(Duration::from_millis(3_200)));
        assert!(text.contains("in 2 repos in 3.2 s\n"), "{text}");
    }

    #[test]
    fn the_colored_render_wraps_the_pieces_in_escape_codes() {
        let text = render_with(&sample(), true, None);
        assert!(text.contains('\u{1b}'));
    }

    #[test]
    fn json_has_the_documented_shape() {
        assert_eq!(
            to_json(&sample()),
            json!({
                "schema": 1,
                "package": "Acme.Util",
                "ecosystem": "nuget",
                "target": "1.5.0",
                "repos": [
                    {"repo": "acme/api", "files": [
                        {"path": "src/A.csproj", "current": "1.2.0", "status": "change"},
                        {"path": "src/B.csproj", "current": "1.5.0", "status": "up_to_date"}
                    ]},
                    {"repo": "acme/web", "files": [
                        {"path": "C.csproj", "current": "2.0.0", "status": "ahead"},
                        {"path": "D.csproj", "current": "[1.0,2.0)", "status": "range"}
                    ]}
                ],
                "dry_run": true
            })
        );
    }

    fn leveled() -> Report {
        let repos = vec![
            RepoRefs {
                repo: "acme/api".to_string(),
                refs: vec![
                    ("A.csproj".to_string(), "1.0.0".to_string()),
                    ("B.csproj".to_string(), "2.0.0".to_string()),
                    ("C.csproj".to_string(), "2.1.0".to_string()),
                    ("D.csproj".to_string(), "2.1.5".to_string()),
                    ("E.csproj".to_string(), "3.0.0".to_string()),
                    ("F.csproj".to_string(), "^1.0".to_string()),
                ],
            },
            RepoRefs {
                repo: "acme/web".to_string(),
                refs: vec![
                    ("G.csproj".to_string(), "2.1.5".to_string()),
                    ("H.csproj".to_string(), "3.0.0".to_string()),
                ],
            },
        ];
        Report::build("Acme.Util", Ecosystem::Nuget, "2.1.5", &repos)
    }

    fn statuses(report: &Report) -> Vec<Status> {
        report
            .repos
            .iter()
            .flat_map(|repo| repo.files.iter().map(|row| row.status))
            .collect()
    }

    #[test]
    fn the_level_is_the_first_number_that_differs() {
        assert_eq!(level_of("1.2.0", "2.0.0"), Level::Major);
        assert_eq!(level_of("1.2.0", "1.3.0"), Level::Minor);
        assert_eq!(level_of("1.2.0", "1.2.5"), Level::Patch);
        assert_eq!(level_of("1.2.0", "1.2.0.1"), Level::Patch);
        assert_eq!(level_of("1.0.0-beta", "1.0.0"), Level::Patch);
        assert_eq!(level_of("1.10", "1.10.1"), Level::Patch);
    }

    #[test]
    fn restrict_leaves_only_changes_of_the_chosen_level() {
        let report = leveled().restrict(&[Level::Minor]);
        assert_eq!(
            statuses(&report),
            vec![
                Status::OtherLevel,
                Status::Change,
                Status::OtherLevel,
                Status::UpToDate,
                Status::Ahead,
                Status::Range,
                Status::UpToDate,
                Status::Ahead,
            ]
        );
    }

    #[test]
    fn restrict_accepts_several_levels() {
        let report = leveled().restrict(&[Level::Major, Level::Patch]);
        assert_eq!(report.count(Status::Change), 2);
        assert_eq!(report.count(Status::OtherLevel), 1);
    }

    #[test]
    fn an_empty_restriction_changes_nothing() {
        assert_eq!(leveled().restrict(&[]), leveled());
    }

    #[test]
    fn the_summary_counts_other_levels_as_left_alone() {
        let text = render(&leveled().restrict(&[Level::Minor]));
        assert!(
            text.contains("1 would change, 2 up to date, 5 left alone in 2 repos"),
            "{text}"
        );
        assert!(text.contains("A.csproj  Acme.Util 1.0.0 different level (left alone)"));
    }

    #[test]
    fn changes_only_lists_just_the_changes_and_keeps_the_summary() {
        let text = render_rows(&leveled(), false, None, true);
        let expected = [
            "bump Acme.Util to 2.1.5",
            "acme/api",
            "  A.csproj  Acme.Util 1.0.0 -> 2.1.5",
            "  B.csproj  Acme.Util 2.0.0 -> 2.1.5",
            "  C.csproj  Acme.Util 2.1.0 -> 2.1.5",
            "3 would change, 2 up to date, 3 left alone in 2 repos",
            "dry run: nothing was written",
            "",
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn changes_only_follows_the_restriction() {
        let text = render_rows(&leveled().restrict(&[Level::Minor]), false, None, true);
        let expected = [
            "bump Acme.Util to 2.1.5",
            "acme/api",
            "  B.csproj  Acme.Util 2.0.0 -> 2.1.5",
            "1 would change, 2 up to date, 5 left alone in 2 repos",
            "dry run: nothing was written",
            "",
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn without_the_new_options_the_render_is_unchanged() {
        let report = leveled();
        assert_eq!(
            render_rows(&report, false, None, false),
            render_with(&report, false, None)
        );
    }

    #[test]
    fn json_without_options_equals_the_plain_json() {
        let report = leveled();
        let json = to_json_with(&report, &[], false);
        assert_eq!(json, to_json(&report));
        assert!(json.get("only").is_none());
    }

    #[test]
    fn json_lists_the_levels_it_was_restricted_to() {
        let report = leveled().restrict(&[Level::Minor]);
        let json = to_json_with(&report, &[Level::Minor], false);
        assert_eq!(json["only"], json!(["minor"]));
        assert_eq!(json["repos"][0]["files"][0]["status"], json!("other_level"));
        assert_eq!(json["repos"][0]["files"][1]["status"], json!("change"));
        assert_eq!(json["repos"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn json_changes_only_drops_other_files_and_repos() {
        let report = leveled().restrict(&[Level::Minor]);
        let json = to_json_with(&report, &[Level::Minor], true);
        assert_eq!(
            json["repos"],
            json!([
                {"repo": "acme/api", "files": [
                    {"path": "B.csproj", "current": "2.0.0", "status": "change"}
                ]}
            ])
        );
        assert_eq!(json["only"], json!(["minor"]));
    }
}
