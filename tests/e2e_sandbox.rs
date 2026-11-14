use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use pipetrain::config::Config;
use pipetrain::merge::{ApiEstimate, Report, StepReport, Store, Summary};
use serde_json::Value;

const API: &str = "lucasbp/pipetrain-sandbox-api";
const WEB: &str = "lucasbp/pipetrain-sandbox-web";
const STATIC: &str = "lucasbp/pipetrain-sandbox-static";
const GUARDED: &str = "lucasbp/pipetrain-sandbox-guarded";
const WORKFLOW: &str = ".github/workflows/deploy.yml";
const POLL: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

fn enabled() -> bool {
    if std::env::var("PIPETRAIN_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set PIPETRAIN_E2E=1 to use the GitHub sandbox");
        return false;
    }
    true
}

fn guard(repo: &str) {
    assert!(
        repo.starts_with("lucasbp/pipetrain-sandbox-"),
        "refusing non-sandbox repository: {repo}"
    );
    let name = repo.strip_prefix("lucasbp/").unwrap();
    assert!(
        name.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._*".contains(&c)),
        "unsafe sandbox repository pattern: {repo}"
    );
}

fn config_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/e2e")
        .join(name)
}

fn validate_config(path: &Path) {
    let guarded = path == config_path("guarded.toml");
    assert!(guarded || path == config_path("sandbox.toml"));
    let text = fs::read_to_string(path).unwrap();
    let raw: toml::Value = toml::from_str(&text).unwrap();
    assert!(raw.get("profile").is_none());
    assert!(raw.get("org_alias").is_none());
    let cfg = Config::parse(&text).expect("sandbox config must parse");
    assert_eq!(cfg.orgs, ["lucasbp"]);
    assert!(cfg.merge.state_file.is_none());
    assert!(cfg.authors.is_empty());
    for repo in cfg
        .layer
        .iter()
        .flat_map(|layer| &layer.repos)
        .chain(cfg.order.iter().flat_map(|order| &order.repos))
        .chain(&cfg.checks_optional_repos)
        .chain(&cfg.deploy.infra_repos)
        .chain(cfg.targets.keys())
        .chain(cfg.packages.values())
        .chain(&cfg.merge.no_deploy_repos)
        .chain(cfg.merge.repo_timeout_minutes.keys())
    {
        guard(repo);
    }
    if guarded {
        assert_eq!(cfg.max_parallel, 1);
        assert!(cfg.require_approval);
        assert_eq!(cfg.layer.len(), 1);
        assert_eq!(cfg.layer[0].repos, [GUARDED]);
        assert_eq!(cfg.merge.no_deploy_repos, [GUARDED]);
        assert_eq!(cfg.api.max_requests, 1000);
        assert_eq!(cfg.api.max_per_minute, 1000);
        assert_eq!(cfg.api.min_remaining, 200);
    } else {
        assert_eq!(cfg.max_parallel, 2);
        assert!(!cfg.require_approval);
        assert_eq!(cfg.merge.no_deploy_repos, [STATIC]);
    }
    assert!(cfg.allow_missing_checks);
    assert_eq!(cfg.merge.poll_seconds, 3);
    assert_eq!(cfg.merge.timeout_minutes, 3);
    assert_eq!(cfg.merge.no_run_grace_seconds, 120);
    assert_eq!(cfg.merge.method, "merge");
}

fn millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn unique_directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "pipetrain-e2e-{}-{}-{}",
        std::process::id(),
        millis(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    directory
}

fn execute(command: &mut Command, timeout: Duration) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start subprocess")?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break match result {
                    Err(error) => Err(error.into()),
                    _ => Err(anyhow::anyhow!("subprocess exceeded {timeout:?}")),
                };
            }
        }
    };
    let stdout = out.join().expect("stdout reader panicked")?;
    let stderr = err.join().expect("stderr reader panicked")?;
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn gh(
    repo: &str,
    method: &str,
    resource: &str,
    fields: &[(&str, &str)],
    timeout: Duration,
) -> Result<Value> {
    guard(repo);
    ensure!(!repo.contains('*'), "API calls require an exact repo");
    let mut command = Command::new("gh");
    command
        .args(["api", "--hostname", "github.com", "-X", method])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_DEBUG")
        .env("GH_PROMPT_DISABLED", "1");
    if resource == "user" {
        ensure!(method == "GET" && fields.is_empty(), "user is read-only");
        command.args(["user", "--jq", ".login"]);
    } else {
        command.arg(format!("repos/{repo}/{resource}"));
    }
    for (key, value) in fields {
        command.args([
            if *key == "force" { "-F" } else { "-f" },
            &format!("{key}={value}"),
        ]);
    }
    let output = execute(&mut command, timeout.min(REQUEST_TIMEOUT))?;
    ensure!(
        output.status.success(),
        "gh {method} {repo}/{resource}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if resource == "user" {
        let login = String::from_utf8(output.stdout)?.trim().to_owned();
        ensure!(!login.is_empty(), "gh returned an empty login");
        Ok(Value::String(login))
    } else if output.stdout.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&output.stdout).context("invalid gh API JSON")
    }
}

fn read(repo: &str, resource: &str) -> Value {
    gh(repo, "GET", resource, &[], REQUEST_TIMEOUT).unwrap()
}

fn baseline(repo: &str) -> String {
    guard(repo);
    let mut command = Command::new("gh");
    command
        .args(["api", "--hostname", "github.com"])
        .arg(format!("repos/{repo}/commits/baseline"))
        .args(["--jq", ".sha"])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_DEBUG")
        .env("GH_PROMPT_DISABLED", "1");
    let output = execute(&mut command, REQUEST_TIMEOUT).unwrap();
    assert_exit(&output, 0);
    let sha = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    assert_sha(&sha);
    sha
}

fn reset_main(repo: &str, sha: &str) -> Result<()> {
    guard(repo);
    gh(
        repo,
        "PATCH",
        "git/refs/heads/main",
        &[("sha", sha), ("force", "true")],
        REQUEST_TIMEOUT,
    )?;
    Ok(())
}

fn main_tip(repo: &str) -> String {
    read(repo, "git/ref/heads/main")["object"]["sha"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn create_branch(repo: &str, branch: &str, sha: &str) {
    guard(repo);
    gh(
        repo,
        "POST",
        "git/refs",
        &[("ref", &format!("refs/heads/{branch}")), ("sha", sha)],
        REQUEST_TIMEOUT,
    )
    .unwrap();
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        encoded.push(ALPHABET[(a >> 2) as usize] as char);
        encoded.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn put_file(repo: &str, branch: &str, path: &str, content: &str, previous_sha: Option<&str>) {
    guard(repo);
    let encoded = base64(content.as_bytes());
    let mut fields = vec![
        ("branch", branch),
        ("message", "sandbox e2e change"),
        ("content", &encoded),
    ];
    if let Some(sha) = previous_sha {
        fields.push(("sha", sha));
    }
    gh(
        repo,
        "PUT",
        &format!("contents/{path}"),
        &fields,
        REQUEST_TIMEOUT,
    )
    .unwrap();
}

fn delete_file(repo: &str, branch: &str, path: &str) {
    guard(repo);
    let file = read(repo, &format!("contents/{path}?ref={branch}"));
    gh(
        repo,
        "DELETE",
        &format!("contents/{path}"),
        &[
            ("branch", branch),
            ("message", "sandbox e2e fix"),
            ("sha", file["sha"].as_str().unwrap()),
        ],
        REQUEST_TIMEOUT,
    )
    .unwrap();
}

#[derive(Clone)]
struct Pull {
    repo: String,
    number: u64,
    branch: String,
}

impl Pull {
    fn reference(&self) -> String {
        guard(&self.repo);
        assert!(self.number > 0);
        format!("{}#{}", self.repo, self.number)
    }
}

fn open_pr(repo: &str, branch: &str) -> Pull {
    guard(repo);
    let pr = gh(
        repo,
        "POST",
        "pulls",
        &[
            ("head", branch),
            ("base", "main"),
            ("title", branch),
            ("body", "Opt-in pipetrain sandbox test"),
        ],
        REQUEST_TIMEOUT,
    )
    .unwrap();
    Pull {
        repo: repo.to_owned(),
        number: pr["number"].as_u64().unwrap(),
        branch: branch.to_owned(),
    }
}

fn close_pr(repo: &str, number: u64) -> Result<()> {
    guard(repo);
    let resource = format!("pulls/{number}");
    if gh(repo, "GET", &resource, &[], REQUEST_TIMEOUT)?["state"] == "open" {
        gh(
            repo,
            "PATCH",
            &resource,
            &[("state", "closed")],
            REQUEST_TIMEOUT,
        )?;
    }
    Ok(())
}

fn delete_branch(repo: &str, branch: &str) -> Result<()> {
    guard(repo);
    gh(
        repo,
        "DELETE",
        &format!("git/refs/heads/{branch}"),
        &[],
        REQUEST_TIMEOUT,
    )?;
    Ok(())
}

fn merge_pr(repo: &str, number: u64) -> String {
    guard(repo);
    let result = gh(
        repo,
        "PUT",
        &format!("pulls/{number}/merge"),
        &[("merge_method", "merge")],
        REQUEST_TIMEOUT,
    )
    .unwrap();
    assert_eq!(result["merged"], true, "{result}");
    let sha = result["sha"].as_str().unwrap().to_owned();
    assert_sha(&sha);
    sha
}

fn wait_clean(pr: &Pull) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "{} did not become clean within 60s",
            pr.reference()
        );
        let reading = gh(
            &pr.repo,
            "GET",
            &format!("pulls/{}", pr.number),
            &[],
            remaining,
        )
        .unwrap();
        if Instant::now() < deadline && reading["mergeable_state"] == "clean" {
            return;
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn wait_blocked(pr: &Pull) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "{} did not become settled and blocked within 60s",
            pr.reference()
        );
        let reading = gh(
            &pr.repo,
            "GET",
            &format!("pulls/{}", pr.number),
            &[],
            remaining,
        )
        .unwrap();
        if Instant::now() < deadline
            && reading["mergeable_state"] == "blocked"
            && reading["mergeable"] == true
        {
            return;
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn check_runs(repo: &str, sha: &str, timeout: Duration) -> Vec<Value> {
    assert_sha(sha);
    let reading = gh(
        repo,
        "GET",
        &format!("commits/{sha}/check-runs?per_page=100&filter=all"),
        &[],
        timeout,
    )
    .unwrap();
    let checks = reading["check_runs"].as_array().unwrap().clone();
    assert_eq!(
        reading["total_count"].as_u64().unwrap(),
        checks.len() as u64
    );
    for check in &checks {
        assert_eq!(check["head_sha"], sha);
    }
    checks
}

fn wait_checks(repo: &str, sha: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "no completed checks within 3 minutes for {repo}@{sha}"
        );
        let checks = check_runs(repo, sha, remaining);
        if Instant::now() < deadline
            && !checks.is_empty()
            && checks.iter().all(|check| check["status"] == "completed")
        {
            return checks
                .iter()
                .map(|check| check["conclusion"].as_str().unwrap().to_owned())
                .collect();
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn runs(repo: &str, sha: &str, timeout: Duration) -> Vec<Value> {
    assert_sha(sha);
    let reading = gh(
        repo,
        "GET",
        &format!("actions/runs?head_sha={sha}&per_page=100"),
        &[],
        timeout,
    )
    .unwrap();
    let runs = reading["workflow_runs"].as_array().unwrap().clone();
    assert_eq!(reading["total_count"].as_u64().unwrap(), runs.len() as u64);
    for run in &runs {
        assert_eq!(run["head_sha"], sha);
    }
    runs
}

fn wait_run(repo: &str, sha: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "no completed deploy within 3 minutes for {repo}@{sha}"
        );
        let reading = runs(repo, sha, remaining);
        assert!(
            reading.len() <= 1,
            "unexpected extra Actions runs: {reading:?}"
        );
        if let Some(run) = reading.first()
            && Instant::now() < deadline
            && run["status"] == "completed"
        {
            assert_eq!(run["path"], WORKFLOW);
            return run["conclusion"].as_str().unwrap().to_owned();
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

struct Sandbox {
    name: String,
    started: Instant,
    directory: PathBuf,
    config: PathBuf,
    baselines: Vec<(String, String)>,
    branches: Vec<(String, String)>,
    prs: Vec<Pull>,
}

impl Sandbox {
    fn new(name: &str, repos: &[&str], config: PathBuf) -> Self {
        validate_config(&config);
        for repo in repos {
            guard(repo);
        }
        let mut sandbox = Self {
            name: name.to_owned(),
            started: Instant::now(),
            directory: unique_directory(),
            config,
            baselines: Vec::new(),
            branches: Vec::new(),
            prs: Vec::new(),
        };
        for repo in repos {
            let sha = baseline(repo);
            sandbox.baselines.push(((*repo).to_owned(), sha.clone()));
            reset_main(repo, &sha).unwrap();
        }
        sandbox
    }

    fn branch(&mut self, repo: &str, phase: &str) -> String {
        guard(repo);
        assert!(self.baselines.iter().any(|(known, _)| known == repo));
        let branch = format!("e2e/{}-{phase}-{}", self.name, millis());
        let sha = main_tip(repo);
        self.branches.push((repo.to_owned(), branch.clone()));
        create_branch(repo, &branch, &sha);
        branch
    }

    fn open(&mut self, repo: &str, branch: &str) -> Pull {
        guard(repo);
        let pr = open_pr(repo, branch);
        self.prs.push(pr.clone());
        wait_clean(&pr);
        pr
    }

    fn open_blocked(&mut self, repo: &str, branch: &str) -> Pull {
        guard(repo);
        let pr = open_pr(repo, branch);
        self.prs.push(pr.clone());
        wait_blocked(&pr);
        pr
    }

    fn cli(&self, args: &[&str], merge: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
        command
            .args(args)
            .arg("--config")
            .arg(&self.config)
            .arg("--state")
            .arg(self.directory.join("state.json"))
            .current_dir(&self.directory)
            .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
            .env("PIPETRAIN_LOG", "0")
            .env("PIPETRAIN_CACHE_DIR", self.directory.join("cache"))
            .env_remove("PIPETRAIN_API_URL")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_DEBUG")
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1");
        if merge {
            command.args(["--no-cache", "--no-progress"]);
        }
        execute(&mut command, Duration::from_secs(240)).unwrap()
    }

    fn merge(&self, prs: &[&Pull], code: i32) -> (Value, Duration) {
        let (report, elapsed) = self.merge_with_admin(prs, code, false);
        assert_eq!(report["applied"], true, "{report}");
        (report, elapsed)
    }

    fn merge_with_admin(&self, prs: &[&Pull], code: i32, admin: bool) -> (Value, Duration) {
        assert!(!prs.is_empty(), "merge requires explicit sandbox PRs");
        let references: Vec<_> = prs
            .iter()
            .map(|pr| {
                guard(&pr.repo);
                assert!(
                    self.prs
                        .iter()
                        .any(|known| known.repo == pr.repo && known.number == pr.number)
                );
                pr.reference()
            })
            .collect();
        let mut args = vec!["merge", "--apply", "--yes", "--json"];
        if admin {
            args.push("--admin");
        }
        args.extend(references.iter().map(String::as_str));
        let started = Instant::now();
        let output = self.cli(&args, true);
        let elapsed = started.elapsed();
        assert_exit(&output, code);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema"], 1);
        if report["steps"].as_array().unwrap().is_empty() {
            assert_eq!(report["applied"], false, "{report}");
            for pr in prs {
                assert!(
                    report["not_merging"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["id"] == pr.reference()),
                    "{report}"
                );
            }
        } else {
            assert_eq!(report["applied"], true, "{report}");
        }
        for step in report["steps"].as_array().unwrap() {
            if step["kind"] == "merge" {
                let pr = prs
                    .iter()
                    .find(|pr| step["target"] == pr.reference())
                    .expect("merge report targeted an unselected PR");
                let live = read(&pr.repo, &format!("pulls/{}", pr.number));
                assert_eq!(live["merged"], true, "{live}");
                assert_eq!(live["merge_commit_sha"], step["merge_sha"]);
                assert_eq!(main_tip(&pr.repo), step["merge_sha"].as_str().unwrap());
            }
        }
        (report, elapsed)
    }

    fn state(&self) -> Value {
        let output = self.cli(&["state", "show", "--json"], false);
        assert_exit(&output, 0);
        let state: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(state["schema"], 2);
        assert_eq!(
            PathBuf::from(state["path"].as_str().unwrap()),
            self.directory.join("state.json")
        );
        state
    }

    fn text(&self) -> String {
        let output = self.cli(&["state", "show"], false);
        assert_exit(&output, 0);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("schema: 2"), "{text}");
        assert!(
            text.contains(&self.directory.join("state.json").display().to_string()),
            "{text}"
        );
        text
    }

    fn settled(&self) {
        assert!(self.state()["pending"].as_array().unwrap().is_empty());
        assert!(self.text().contains("audit:"));
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for pr in &self.prs {
            if let Err(error) = close_pr(&pr.repo, pr.number) {
                eprintln!("cleanup close {}: {error:#}", pr.reference());
            }
        }
        for (repo, branch) in &self.branches {
            if !self
                .prs
                .iter()
                .any(|pr| pr.repo == *repo && pr.branch == *branch)
            {
                match gh(
                    repo,
                    "GET",
                    &format!("pulls?state=open&head=lucasbp:{branch}&base=main"),
                    &[],
                    REQUEST_TIMEOUT,
                ) {
                    Ok(prs) => {
                        if let Some(prs) = prs.as_array() {
                            for pr in prs {
                                if let Some(number) = pr["number"].as_u64()
                                    && let Err(error) = close_pr(repo, number)
                                {
                                    eprintln!("cleanup close {repo}#{number}: {error:#}");
                                }
                            }
                        } else {
                            eprintln!(
                                "cleanup PR lookup returned an invalid list for {repo}:{branch}"
                            );
                        }
                    }
                    Err(error) => eprintln!("cleanup PR lookup {repo}:{branch}: {error:#}"),
                }
            }
            if let Err(error) = delete_branch(repo, branch) {
                eprintln!("cleanup delete {repo}:{branch}: {error:#}");
            }
        }
        for (repo, sha) in &self.baselines {
            if let Err(error) = reset_main(repo, sha) {
                eprintln!("cleanup reset {repo}: {error:#}");
            }
        }
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            eprintln!("cleanup temp directory: {error}");
        }
        eprintln!(
            "{} wall time (including cleanup): {:?}",
            self.name,
            self.started.elapsed()
        );
    }
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_sha(sha: &str) {
    assert_eq!(sha.len(), 40, "invalid commit SHA");
    assert!(sha.bytes().all(|c| c.is_ascii_hexdigit()));
}

fn step<'a>(report: &'a Value, kind: &str, pr: &Pull, status: &str) -> &'a Value {
    let steps: Vec<_> = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|step| step["kind"] == kind && step["target"] == pr.reference())
        .collect();
    assert_eq!(steps.len(), 1, "{report}");
    let step = steps[0];
    assert_eq!(step["status"], status, "{step}");
    assert!(step["detail"].as_str().unwrap().contains("main"), "{step}");
    assert_sha(step["merge_sha"].as_str().unwrap());
    step
}

fn confirmed(step: &Value) {
    assert_eq!(step["deploy_prediction"]["status"], "expected");
    assert!(
        step["deploy_prediction"]["workflows"]
            .as_array()
            .unwrap()
            .contains(&Value::from(WORKFLOW))
    );
    assert_eq!(step["deploy_outcome"], "confirmed");
    let runs = step["expected_runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["path"], WORKFLOW);
    assert_eq!(runs[0]["status"], "completed");
    assert_eq!(runs[0]["conclusion"], "success");
}

fn rendered(report: &Value, sandbox: &Sandbox) -> String {
    let steps: Vec<StepReport> = serde_json::from_value(report["steps"].clone()).unwrap();
    let number = |key: &str| report["api_estimate"][key].as_u64().unwrap();
    let report = Report {
        api_estimate: ApiEstimate {
            discovery: number("discovery"),
            checks: number("checks"),
            deploy_polls: number("deploy_polls"),
            total: number("total"),
            budget: number("budget").try_into().unwrap(),
            exceeds_budget: report["api_estimate"]["exceeds_budget"].as_bool().unwrap(),
        },
        max_parallel: report["max_parallel"].as_u64().unwrap().try_into().unwrap(),
        summary: Summary::from_steps(&steps),
        preflight: Vec::new(),
        state: Store::open(Some(sandbox.directory.join("state.json")))
            .unwrap()
            .preview(Some("default"), 24),
        bump_enabled: false,
        bump_candidates: Vec::new(),
        schema: 1,
        applied: report["applied"].as_bool().unwrap(),
        requested: true,
        stopped: None,
        steps,
        not_merging: Vec::new(),
        questions: Vec::new(),
    };
    pipetrain::command::merge::render(&report, false)
}

#[test]
#[ignore]
fn e2e_api_outside_deploy_paths() {
    if !enabled() {
        return;
    }
    guard(API);
    let mut sandbox = Sandbox::new("api-paths", &[API], config_path("sandbox.toml"));
    let branch = sandbox.branch(API, "change");
    let path = format!("notes/{}.md", millis());
    put_file(API, &branch, &path, "outside deploy paths\n", None);
    let pr = sandbox.open(API, &branch);
    let (report, elapsed) = sandbox.merge(&[&pr], 0);
    assert!(
        elapsed < Duration::from_secs(60),
        "grace was waited: {elapsed:?}"
    );
    let step = step(&report, "merge", &pr, "done");
    assert_eq!(step["deploy_prediction"]["status"], "dispensed");
    assert_eq!(step["deploy_outcome"], "dispensed");
    assert_eq!(report["summary"]["deploy_dispensed"], 1);
    assert!(
        step["deploy_prediction"]["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|exclusion| {
                exclusion["workflow"] == WORKFLOW
                    && exclusion["filter"] == "paths"
                    && exclusion["files"]
                        .as_array()
                        .unwrap()
                        .contains(&Value::from(path.clone()))
            })
    );
    std::thread::sleep(Duration::from_secs(6));
    assert!(runs(API, step["merge_sha"].as_str().unwrap(), REQUEST_TIMEOUT).is_empty());
    sandbox.settled();
}

#[test]
#[ignore]
fn e2e_api_expected_deploy() {
    if !enabled() {
        return;
    }
    guard(API);
    let mut sandbox = Sandbox::new("api-deploy", &[API], config_path("sandbox.toml"));
    let branch = sandbox.branch(API, "change");
    let file = read(API, &format!("contents/deploy/version.txt?ref={branch}"));
    put_file(
        API,
        &branch,
        "deploy/version.txt",
        &format!("{}\n", millis()),
        Some(file["sha"].as_str().unwrap()),
    );
    let pr = sandbox.open(API, &branch);
    let (report, _) = sandbox.merge(&[&pr], 0);
    let step = step(&report, "merge", &pr, "done");
    confirmed(step);
    assert_eq!(report["summary"]["deploy_confirmed"], 1);
    assert_eq!(
        wait_run(API, step["merge_sha"].as_str().unwrap()),
        "success"
    );
    sandbox.settled();
}

#[test]
#[ignore]
fn e2e_web_later_deploy_recovery() {
    if !enabled() {
        return;
    }
    guard(WEB);
    let mut sandbox = Sandbox::new("web-recovery", &[WEB], config_path("sandbox.toml"));
    let branch_a = sandbox.branch(WEB, "failure");
    put_file(WEB, &branch_a, "FAIL_DEPLOY", "fail this deploy\n", None);
    let a = sandbox.open(WEB, &branch_a);
    let (failed, _) = sandbox.merge(&[&a], 1);
    let first = step(&failed, "merge", &a, "failed");
    assert_eq!(first["deploy_outcome"], "failed");
    assert_eq!(failed["summary"]["deploy_failed"], 1);
    assert_eq!(
        wait_run(WEB, first["merge_sha"].as_str().unwrap()),
        "failure"
    );
    let pending = sandbox.state();
    assert_eq!(pending["pending"].as_array().unwrap().len(), 1);
    assert_eq!(pending["pending"][0]["id"], a.reference());
    assert_eq!(pending["pending"][0]["sha"], first["merge_sha"]);
    assert!(
        sandbox
            .text()
            .contains(&format!("pending {}:", a.reference()))
    );
    let branch_b = sandbox.branch(WEB, "fix");
    delete_file(WEB, &branch_b, "FAIL_DEPLOY");
    let b = sandbox.open(WEB, &branch_b);
    let fix_sha = merge_pr(WEB, b.number);
    assert_eq!(wait_run(WEB, &fix_sha), "success");
    let (resumed, _) = sandbox.merge(&[&b], 0);
    let resume = resumed["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["kind"] == "resume")
        .unwrap();
    assert_eq!(resume["target"], a.reference());
    assert_eq!(resume["status"], "done");
    assert_eq!(resume["merge_sha"], first["merge_sha"]);
    assert_eq!(resume["resolution"]["status"], "resolved_by_later_deploy");
    assert_eq!(resume["resolution"]["sha"], fix_sha);
    assert_eq!(resumed["summary"]["deploy_resolved"], 1);
    assert_eq!(resumed["summary"]["merged"], 0);
    assert!(
        resumed["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["kind"] != "merge")
    );
    assert!(
        resumed["not_merging"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == b.reference())
    );
    let state = sandbox.state();
    assert!(state["pending"].as_array().unwrap().is_empty());
    assert_eq!(state["audit"].as_array().unwrap().len(), 1);
    assert_eq!(state["audit"][0]["id"], a.reference());
    assert_eq!(state["audit"][0]["action"], "resolved_by_later_deploy");
    assert_eq!(state["audit"][0]["resolution"], resume["resolution"]);
    assert!(sandbox.text().contains("resolved_by_later_deploy"));
}

#[test]
#[ignore]
fn e2e_static_no_deploy() {
    if !enabled() {
        return;
    }
    guard(STATIC);
    let mut sandbox = Sandbox::new("static", &[STATIC], config_path("sandbox.toml"));
    let branch = sandbox.branch(STATIC, "change");
    put_file(STATIC, &branch, "e2e.txt", "no Actions deploy\n", None);
    let pr = sandbox.open(STATIC, &branch);
    let (report, elapsed) = sandbox.merge(&[&pr], 0);
    assert!(
        elapsed < Duration::from_secs(60),
        "grace was waited: {elapsed:?}"
    );
    let step = step(&report, "merge", &pr, "done");
    assert_eq!(step["deploy_outcome"], "dispensed");
    assert_eq!(step["deploy_prediction"]["status"], "dispensed");
    assert!(
        step["deploy_prediction"]["reason"]
            .as_str()
            .unwrap()
            .contains("merge.no_deploy_repos")
    );
    assert_eq!(report["summary"]["deploy_dispensed"], 1);
    sandbox.settled();
}

#[test]
#[ignore]
fn e2e_parallel_web_and_static() {
    if !enabled() {
        return;
    }
    guard(WEB);
    guard(STATIC);
    let mut sandbox = Sandbox::new("parallel", &[WEB, STATIC], config_path("sandbox.toml"));
    let web_branch = sandbox.branch(WEB, "change");
    put_file(WEB, &web_branch, "e2e.txt", "successful deploy\n", None);
    let web = sandbox.open(WEB, &web_branch);
    let static_branch = sandbox.branch(STATIC, "change");
    put_file(STATIC, &static_branch, "e2e.txt", "no deploy\n", None);
    let static_pr = sandbox.open(STATIC, &static_branch);
    let (report, _) = sandbox.merge(&[&web, &static_pr], 0);
    assert_eq!(report["steps"].as_array().unwrap().len(), 2);
    let web_step = step(&report, "merge", &web, "done");
    confirmed(web_step);
    let static_step = step(&report, "merge", &static_pr, "done");
    assert_eq!(static_step["deploy_outcome"], "dispensed");
    assert_ne!(web_step["detail"], static_step["detail"]);
    assert_eq!(report["summary"]["merged"], 2);
    assert_eq!(report["summary"]["deploy_confirmed"], 1);
    assert_eq!(report["summary"]["deploy_dispensed"], 1);
    assert_eq!(report["max_parallel"], 2);
    assert_eq!(
        wait_run(WEB, web_step["merge_sha"].as_str().unwrap()),
        "success"
    );
    sandbox.settled();
    let text = rendered(&report, &sandbox);
    assert!(text.contains(&web.reference()), "{text}");
    assert!(text.contains(&static_pr.reference()), "{text}");
    assert!(text.contains("deploys: 1 confirmed, 1 dispensed, 0 indeterminate, 0 resolved by later deploy, 0 skipped, 0 failed"), "{text}");
}

fn refused(report: &Value, sandbox: &Sandbox, pr: &Pull) -> Vec<String> {
    assert!(report["steps"].as_array().unwrap().is_empty(), "{report}");
    assert_eq!(report["summary"]["merged"], 0);
    let excluded: Vec<_> = report["not_merging"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["id"] == pr.reference())
        .collect();
    assert_eq!(excluded.len(), 1, "{report}");
    let live = read(&pr.repo, &format!("pulls/{}", pr.number));
    assert_eq!(live["state"], "open", "{live}");
    assert_eq!(live["merged"], false, "{live}");
    let (_, baseline) = sandbox
        .baselines
        .iter()
        .find(|(repo, _)| repo == &pr.repo)
        .unwrap();
    assert_eq!(main_tip(&pr.repo), *baseline);
    excluded[0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| reason.as_str().unwrap().to_owned())
        .collect()
}

#[test]
#[ignore]
fn e2e_guarded_refused_without_admin() {
    if !enabled() {
        return;
    }
    guard(GUARDED);
    let mut sandbox = Sandbox::new("guarded-refused", &[GUARDED], config_path("guarded.toml"));
    let branch = sandbox.branch(GUARDED, "change");
    put_file(GUARDED, &branch, "e2e.txt", "review required\n", None);
    let pr = sandbox.open_blocked(GUARDED, &branch);
    let live = read(GUARDED, &format!("pulls/{}", pr.number));
    let sha = live["head"]["sha"].as_str().unwrap();
    assert!(check_runs(GUARDED, sha, REQUEST_TIMEOUT).is_empty());
    let (report, _) = sandbox.merge_with_admin(&[&pr], 0, false);
    let reasons = refused(&report, &sandbox, &pr);
    assert!(
        reasons
            .iter()
            .any(|reason| reason.to_ascii_lowercase().contains("review")
                || reason.contains("BLOCKED")),
        "{reasons:?}"
    );
    assert!(check_runs(GUARDED, sha, REQUEST_TIMEOUT).is_empty());
}

#[test]
#[ignore]
fn e2e_guarded_admin_override() {
    if !enabled() {
        return;
    }
    guard(GUARDED);
    let mut sandbox = Sandbox::new("guarded-override", &[GUARDED], config_path("guarded.toml"));
    let branch = sandbox.branch(GUARDED, "change");
    put_file(GUARDED, &branch, "e2e.txt", "admin review override\n", None);
    let pr = sandbox.open_blocked(GUARDED, &branch);
    let live = read(GUARDED, &format!("pulls/{}", pr.number));
    let sha = live["head"]["sha"].as_str().unwrap();
    assert!(check_runs(GUARDED, sha, REQUEST_TIMEOUT).is_empty());
    let login = gh(GUARDED, "GET", "user", &[], REQUEST_TIMEOUT).unwrap();
    let (report, _) = sandbox.merge_with_admin(&[&pr], 0, true);
    let step = step(&report, "merge", &pr, "done");
    let admin = &step["admin_override"];
    assert_eq!(admin["reason"], "REVIEW_REQUIRED");
    assert_eq!(admin["requested_by"], login);
    chrono::DateTime::parse_from_rfc3339(admin["at"].as_str().unwrap()).unwrap();
    let merged = read(GUARDED, &format!("pulls/{}", pr.number));
    assert_eq!(merged["merged"], true, "{merged}");
    assert_eq!(merged["merged_by"]["login"], login);
    assert_eq!(step["deploy_outcome"], "dispensed");
    let state = sandbox.state();
    let overrides = state["admin_overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1, "{state}");
    assert_eq!(overrides[0]["id"], pr.reference());
    assert_eq!(overrides[0]["sha"], step["merge_sha"]);
    assert_eq!(overrides[0]["reason"], admin["reason"]);
    assert_eq!(overrides[0]["requested_by"], admin["requested_by"]);
    assert_eq!(overrides[0]["at"], admin["at"]);
    assert!(sandbox.text().contains("admin override"));
    assert!(check_runs(GUARDED, sha, REQUEST_TIMEOUT).is_empty());
    sandbox.settled();
}

#[test]
#[ignore]
fn e2e_guarded_admin_refuses_failing_check() {
    if !enabled() {
        return;
    }
    guard(GUARDED);
    let mut sandbox = Sandbox::new("guarded-failure", &[GUARDED], config_path("guarded.toml"));
    let branch = sandbox.branch(GUARDED, "change");
    put_file(
        GUARDED,
        &branch,
        ".github/workflows/fail.yml",
        "name: fail\non: pull_request\njobs:\n  fail:\n    runs-on: ubuntu-latest\n    steps:\n      - run: exit 1\n",
        None,
    );
    put_file(GUARDED, &branch, "e2e.txt", "failing check\n", None);
    let pr = sandbox.open_blocked(GUARDED, &branch);
    let live = read(GUARDED, &format!("pulls/{}", pr.number));
    let sha = live["head"]["sha"].as_str().unwrap();
    let conclusions = wait_checks(GUARDED, sha);
    assert_eq!(conclusions.len(), 1, "{conclusions:?}");
    assert_eq!(conclusions, ["failure"]);
    let (report, _) = sandbox.merge_with_admin(&[&pr], 0, true);
    let reasons = refused(&report, &sandbox, &pr);
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("checks failing")),
        "{reasons:?}"
    );
}
