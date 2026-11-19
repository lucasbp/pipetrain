use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Result, bail};
use pipetrain::consumer_bump::apply::{Applied, Candidate, apply_repo};
use pipetrain::consumer_bump::{BumpGateway, Consumer, RestoreMode, RestoreSettings};
use pipetrain::feed::Ecosystem;
use serde_json::{Value, json};

mod common;

use common::{Server, TempDir, serve_requests};

const TOKEN: &str = "ghp_FAKE0123456789";
const NPM_OLD: &str =
    "{\n  \"name\": \"web\",\n  \"dependencies\": {\n    \"@acme/ui\": \"1.0.0\"\n  }\n}\n";
const BRANCH: &str = "pipetrain/bump/acme-ui-2.0.0";

fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}/", text.trim_start_matches('/'))
}

struct Repo {
    head: String,
    files: BTreeMap<String, String>,
}

fn make_remote(remotes: &Path, work: &Path, slug: &str, files: &[(&str, &str)]) -> Repo {
    let bare = remotes.join(format!("{slug}.git"));
    fs::create_dir_all(&bare).unwrap();
    git_in(&bare, &["init", "--bare", "-b", "main"]);
    let clone = work.join(slug.replace('/', "-"));
    fs::create_dir_all(&clone).unwrap();
    git_in(&clone, &["init", "-b", "main"]);
    let mut kept = BTreeMap::new();
    for (path, text) in files {
        let target = clone.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, text).unwrap();
        kept.insert(path.to_string(), text.to_string());
    }
    git_in(&clone, &["add", "."]);
    git_in(
        &clone,
        &["-c", "commit.gpgsign=false", "commit", "-m", "init"],
    );
    git_in(
        &clone,
        &["push", bare.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    Repo {
        head: git_in(&clone, &["rev-parse", "HEAD"]),
        files: kept,
    }
}

struct Env {
    dir: TempDir,
    server: Server,
}

impl Env {
    fn bare(&self, slug: &str) -> PathBuf {
        self.dir.join("remotes").join(format!("{slug}.git"))
    }

    fn seen(&self) -> Vec<String> {
        self.server.seen.lock().unwrap().clone()
    }

    fn lines(&self) -> Vec<String> {
        self.seen()
            .iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(" HTTP/1.1")
                    .to_string()
            })
            .collect()
    }

    fn writes(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line.starts_with("POST /repos/"))
            .collect()
    }

    fn branch_exists(&self, slug: &str) -> bool {
        Command::new("git")
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{BRANCH}"))
            .current_dir(self.bare(slug))
            .output()
            .unwrap()
            .status
            .success()
    }
}

struct State {
    repos: BTreeMap<String, Repo>,
    versions: Vec<String>,
    refuse_refs: Vec<String>,
    remotes: PathBuf,
}

fn blob_fields(repo: &Repo, variables: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    let mut index = 0;
    while let Some(expression) = variables[format!("e{index}")].as_str() {
        let path = expression.split_once(':').map_or("", |(_, path)| path);
        let text = repo.files.get(path).cloned().unwrap_or_default();
        fields.insert(format!("f{index}"), json!({ "text": text }));
        index += 1;
    }
    Value::Object(fields)
}

fn route(state: &State, line: &str, payload: &str) -> (u16, Value) {
    let (method, rest) = line.split_once(' ').unwrap();
    let path = rest.split('?').next().unwrap();
    if path.contains("%2fui") {
        let versions: BTreeMap<&str, Value> = state
            .versions
            .iter()
            .map(|version| (version.as_str(), json!({})))
            .collect();
        return (200, json!({ "versions": versions }));
    }
    if method == "POST" && path == "/graphql" {
        let body: Value = serde_json::from_str(payload).unwrap();
        let variables = &body["variables"];
        let slug = format!(
            "{}/{}",
            variables["o"].as_str().unwrap_or_default(),
            variables["r"].as_str().unwrap_or_default()
        );
        let Some(repo) = state.repos.get(&slug) else {
            return (200, json!({"data": {"repository": null}}));
        };
        if body["query"]
            .as_str()
            .unwrap_or_default()
            .contains("defaultBranchRef")
        {
            return (
                200,
                json!({"data": {"repository": {"defaultBranchRef": {"target": {"oid": repo.head}}}}}),
            );
        }
        return (
            200,
            json!({"data": {"repository": blob_fields(repo, variables)}}),
        );
    }
    let Some(tail) = path.strip_prefix("/repos/") else {
        return (404, json!({"message": "Not Found"}));
    };
    let mut parts = tail.splitn(3, '/');
    let slug = format!(
        "{}/{}",
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default()
    );
    let rest = parts.next().unwrap_or_default();
    let Some(repo) = state.repos.get(&slug) else {
        return (404, json!({"message": "Not Found"}));
    };
    match (method, rest) {
        ("GET", "") => (200, json!({"default_branch": "main"})),
        ("GET", tree) if tree.starts_with("git/trees/") => {
            let entries: Vec<Value> = repo
                .files
                .keys()
                .map(|path| json!({"type": "blob", "path": path}))
                .collect();
            (200, json!({"tree": entries, "truncated": false}))
        }
        ("GET", "git/ref/heads/main") => (200, json!({"object": {"sha": repo.head}})),
        ("GET", reference) if reference.starts_with("git/ref/heads/") => {
            let branch = reference
                .trim_start_matches("git/ref/heads/")
                .replace("%2F", "/")
                .replace("%2f", "/");
            let found = Command::new("git")
                .args(["rev-parse", "--verify", "--quiet"])
                .arg(format!("refs/heads/{branch}"))
                .current_dir(state.remotes.join(format!("{slug}.git")))
                .output()
                .unwrap();
            if found.status.success() {
                let sha = String::from_utf8_lossy(&found.stdout).trim().to_string();
                (200, json!({"object": {"sha": sha}}))
            } else {
                (404, json!({"message": "Not Found"}))
            }
        }
        ("POST", "git/refs") if state.refuse_refs.contains(&slug) => {
            (422, json!({"message": "Validation Failed"}))
        }
        ("POST", "git/refs") => (201, json!({"ref": "refs/heads/created"})),
        ("POST", "pulls") => (
            201,
            json!({"html_url": format!("https://github.com/{slug}/pull/1")}),
        ),
        _ => (404, json!({"message": "Not Found"})),
    }
}

fn setup(repos: &[(&str, Vec<(&str, &str)>)], versions: &[&str], refuse_refs: &[&str]) -> Env {
    let dir = TempDir::new("bump-apply");
    let remotes = dir.join("remotes");
    let work = dir.join("work");
    fs::create_dir_all(&remotes).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::write(dir.join("pipetrain.toml"), "orgs = [\"acme\"]\n").unwrap();
    fs::write(
        dir.join("gitconfig"),
        format!(
            "[url \"{}\"]\n\tinsteadOf = https://github.com/\n[user]\n\tname = Test\n\temail = test@example.com\n[commit]\n\tgpgsign = false\n",
            file_url(&remotes)
        ),
    )
    .unwrap();
    let mut made = BTreeMap::new();
    for (slug, files) in repos {
        made.insert(slug.to_string(), make_remote(&remotes, &work, slug, files));
    }
    let state = State {
        repos: made,
        versions: versions.iter().map(|v| v.to_string()).collect(),
        refuse_refs: refuse_refs.iter().map(|s| s.to_string()).collect(),
        remotes,
    };
    let server = serve_requests(move |line, payload| route(&state, line, payload));
    Env { dir, server }
}

fn bump(env: &Env, args: &[&str]) -> Output {
    let config = env.dir.join("pipetrain.toml");
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .arg("bump")
        .args(args)
        .args(["--config", config.to_str().unwrap()])
        .args(["--registry", &env.server.base])
        .env("PIPETRAIN_API_URL", &env.server.base)
        .env("PIPETRAIN_CACHE_DIR", env.dir.join("cache"))
        .env("GIT_CONFIG_GLOBAL", env.dir.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_TOKEN", TOKEN)
        .env_remove("GITHUB_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn web() -> Vec<(&'static str, &'static str)> {
    vec![("web/package.json", NPM_OLD), ("README.md", "web")]
}

const TO: [&str; 2] = ["--to", "2.0.0"];
const BASE: [&str; 3] = ["@acme/ui", "--ecosystem", "npm"];

fn args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut all: Vec<&str> = BASE.to_vec();
    all.extend_from_slice(&TO);
    all.extend_from_slice(extra);
    all
}

#[test]
fn without_apply_nothing_is_written_and_the_json_is_the_dry_run() {
    let env = setup(&[("acme/web", web())], &["1.0.0", "2.0.0"], &[]);
    let output = bump(&env, &args(&["--repo", "acme/web", "--json"]));
    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["dry_run"], json!(true));
    assert_eq!(json["repos"][0]["files"][0]["status"], json!("change"));
    assert!(json.get("applied").is_none());
    assert!(env.writes().is_empty(), "{:?}", env.writes());
    assert!(!env.branch_exists("acme/web"));
}

#[test]
fn apply_opens_one_draft_pr_from_an_isolated_branch_changing_only_the_listed_manifest() {
    let env = setup(&[("acme/web", web())], &["1.0.0", "2.0.0"], &[]);
    let output = bump(
        &env,
        &args(&["--repo", "acme/web", "--apply", "--yes", "--json"]),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["applied"], json!(true));
    assert_eq!(json["repos"][0]["status"], json!("opened"));
    assert_eq!(
        json["repos"][0]["pull_request_url"],
        json!("https://github.com/acme/web/pull/1")
    );
    assert_eq!(json["repos"][0]["branch"], json!(BRANCH));

    let bare = env.bare("acme/web");
    assert!(env.branch_exists("acme/web"));
    assert_eq!(
        git_in(&bare, &["diff", "--name-only", "main", BRANCH]),
        "web/package.json"
    );
    let changed = git_in(&bare, &["show", &format!("{BRANCH}:web/package.json")]);
    assert_eq!(changed, NPM_OLD.replace("1.0.0", "2.0.0").trim_end());
    assert_eq!(
        git_in(&bare, &["log", "-1", "--format=%s", BRANCH]),
        "bump @acme/ui to 2.0.0"
    );
    assert_eq!(
        git_in(&bare, &["rev-list", "--count", &format!("main..{BRANCH}")]),
        "1"
    );

    let writes = env.writes();
    assert_eq!(
        writes,
        vec![
            "POST /repos/acme/web/git/refs".to_string(),
            "POST /repos/acme/web/pulls".to_string()
        ]
    );
    let pull = env
        .seen()
        .into_iter()
        .find(|request| request.starts_with("POST /repos/acme/web/pulls"))
        .unwrap();
    let body: Value = serde_json::from_str(pull.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["draft"], json!(true));
    assert_eq!(body["base"], json!("main"));
    assert_eq!(body["head"], json!(BRANCH));
    assert_eq!(body["title"], json!("Bump @acme/ui to 2.0.0"));
    let text = body["body"].as_str().unwrap();
    assert!(
        text.contains("pipetrain opened this draft pull request"),
        "{text}"
    );
    assert!(text.contains("`@acme/ui` to 2.0.0"), "{text}");
    assert!(
        text.contains("`web/package.json`: 1.0.0 -> 2.0.0"),
        "{text}"
    );
}

#[test]
fn apply_needs_yes_without_a_terminal_and_touches_nothing() {
    let env = setup(&[("acme/web", web())], &["1.0.0", "2.0.0"], &[]);
    let output = bump(&env, &args(&["--repo", "acme/web", "--apply"]));
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("refusing to write without a terminal: pass --yes"),
        "{}",
        stderr(&output)
    );
    assert!(env.seen().is_empty());
}

#[test]
fn apply_stops_before_any_write_when_the_version_is_not_in_the_feed() {
    let env = setup(&[("acme/web", web())], &["1.0.0"], &[]);
    let output = bump(&env, &args(&["--repo", "acme/web", "--apply", "--yes"]));
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("@acme/ui 2.0.0 is not in the package feed yet"),
        "{}",
        stderr(&output)
    );
    assert!(env.writes().is_empty());
    assert!(!env.branch_exists("acme/web"));
}

#[test]
fn a_repo_with_an_npm_lock_file_is_skipped_and_reported() {
    let locked = vec![("web/package.json", NPM_OLD), ("package-lock.json", "{}")];
    let env = setup(
        &[("acme/locked", locked), ("acme/web", web())],
        &["1.0.0", "2.0.0"],
        &[],
    );
    let output = bump(
        &env,
        &args(&[
            "--repo",
            "acme/locked",
            "--repo",
            "acme/web",
            "--apply",
            "--yes",
            "--json",
        ]),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["repos"][0]["repo"], json!("acme/locked"));
    assert_eq!(json["repos"][0]["status"], json!("skipped"));
    assert!(
        json["repos"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("package-lock.json")
    );
    assert_eq!(json["repos"][1]["status"], json!("opened"));
    assert!(!env.branch_exists("acme/locked"));
    assert!(env.branch_exists("acme/web"));
}

#[test]
fn the_first_failure_stops_the_rest_and_the_report_says_what_was_skipped() {
    let env = setup(
        &[("acme/aaa", web()), ("acme/web", web())],
        &["1.0.0", "2.0.0"],
        &["acme/aaa"],
    );
    let output = bump(
        &env,
        &args(&[
            "--repo", "acme/aaa", "--repo", "acme/web", "--apply", "--yes", "--json",
        ]),
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["repos"][0]["repo"], json!("acme/aaa"));
    assert_eq!(json["repos"][0]["status"], json!("failed"));
    assert_eq!(json["repos"][1]["repo"], json!("acme/web"));
    assert_eq!(json["repos"][1]["status"], json!("skipped"));
    assert_eq!(
        json["repos"][1]["detail"],
        json!("stopped after an earlier failure")
    );
    assert!(!env.branch_exists("acme/aaa"));
    assert!(!env.branch_exists("acme/web"));
    assert!(
        env.writes().iter().all(|line| line.contains("acme/aaa")),
        "{:?}",
        env.writes()
    );
}

#[test]
fn an_existing_bump_branch_is_never_touched() {
    let env = setup(&[("acme/web", web())], &["1.0.0", "2.0.0"], &[]);
    let first = bump(&env, &args(&["--repo", "acme/web", "--apply", "--yes"]));
    assert!(first.status.success(), "{}", stderr(&first));
    let tip = git_in(&env.bare("acme/web"), &["rev-parse", BRANCH]);
    let second = bump(
        &env,
        &args(&["--repo", "acme/web", "--apply", "--yes", "--json"]),
    );
    assert!(second.status.success(), "{}", stderr(&second));
    let json: Value = serde_json::from_str(&stdout(&second)).unwrap();
    assert_eq!(json["repos"][0]["status"], json!("skipped"));
    assert!(
        json["repos"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );
    assert_eq!(git_in(&env.bare("acme/web"), &["rev-parse", BRANCH]), tip);
    assert_eq!(env.writes().len(), 2);
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";
const CSPROJ: &str = "src/Api/Api.csproj";
const LOCK: &str = "src/Api/packages.lock.json";

fn csproj(version: &str) -> String {
    format!(
        r#"<Project><ItemGroup><PackageReference Include="Acme.Util" Version="{version}" /></ItemGroup></Project>"#
    )
}

fn lock(version: &str) -> String {
    json!({"version": 2, "dependencies": {"net8.0": {"Acme.Util": {"type": "Direct", "resolved": version}}}})
        .to_string()
}

struct Fake {
    original: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
    head: String,
    resolves_lock: bool,
    build_fails: bool,
    log: Vec<String>,
    committed: Vec<String>,
    body: String,
}

impl Fake {
    fn nuget(version: &str) -> Self {
        let files: BTreeMap<String, String> = [
            (CSPROJ.to_string(), csproj(version)),
            (LOCK.to_string(), lock(version)),
        ]
        .into();
        Self {
            original: files.clone(),
            files,
            head: HEAD.to_string(),
            resolves_lock: true,
            build_fails: false,
            log: Vec::new(),
            committed: Vec::new(),
            body: String::new(),
        }
    }
}

impl BumpGateway for Fake {
    fn consumer_head(&mut self, _: &Consumer) -> Result<String> {
        Ok(self.head.clone())
    }
    fn create_worktree(&mut self, _: &Consumer) -> Result<()> {
        self.log.push("worktree".into());
        Ok(())
    }
    fn read(&mut self, path: &str) -> Result<String> {
        match self.files.get(path) {
            Some(text) => Ok(text.clone()),
            None => bail!("no such file {path}"),
        }
    }
    fn write(&mut self, path: &str, text: &str) -> Result<()> {
        self.log.push(format!("write {path}"));
        self.files.insert(path.to_string(), text.to_string());
        Ok(())
    }
    fn restore(&mut self, solution: &str, locked: bool) -> Result<()> {
        self.log.push(format!("restore {solution} locked={locked}"));
        if !locked && self.resolves_lock {
            let version = self.files[CSPROJ]
                .split("Version=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap()
                .to_string();
            self.files.insert(LOCK.to_string(), lock(&version));
        }
        Ok(())
    }
    fn restore_plain(&mut self, solution: &str) -> Result<()> {
        self.log.push(format!("restore-plain {solution}"));
        Ok(())
    }
    fn build(&mut self, solution: &str) -> Result<()> {
        self.log.push(format!("build {solution}"));
        if self.build_fails {
            bail!("build failed");
        }
        Ok(())
    }
    fn changed_files(&mut self) -> Result<Vec<String>> {
        Ok(self
            .files
            .iter()
            .filter(|(path, text)| self.original.get(*path) != Some(*text))
            .map(|(path, _)| path.clone())
            .collect())
    }
    fn commit(&mut self, paths: &[String], message: &str) -> Result<String> {
        self.log.push(format!("commit {message}"));
        self.committed = paths.to_vec();
        Ok(COMMIT.to_string())
    }
    fn push_new_branch(&mut self, branch: &str, commit: &str) -> Result<()> {
        assert_eq!(commit, COMMIT);
        self.log.push(format!("push {branch}"));
        Ok(())
    }
    fn open_review_pr(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String> {
        assert_eq!(commit, COMMIT);
        assert_eq!(consumer.head_branch, "main");
        assert_eq!(consumer.default_branch, "main");
        self.log.push("open".into());
        self.body = body.to_string();
        Ok("https://github.com/acme/api/pull/9".into())
    }
}

fn tree(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| path.to_string()).collect()
}

fn rows(current: &str) -> Vec<(String, String)> {
    vec![(CSPROJ.to_string(), current.to_string())]
}

fn run_nuget_with(
    fake: &mut Fake,
    tree: &[String],
    rows: &[(String, String)],
    settings: &RestoreSettings,
) -> Result<Applied> {
    apply_repo(
        Ecosystem::Nuget,
        "Acme.Util",
        "2.0.0",
        &Candidate {
            repo: "acme/api",
            default_branch: "main",
            head_sha: HEAD,
            tree,
            rows,
        },
        settings,
        fake,
    )
}

fn run_nuget(fake: &mut Fake, tree: &[String], rows: &[(String, String)]) -> Result<Applied> {
    run_nuget_with(fake, tree, rows, &RestoreSettings::default())
}

fn settings(mode: RestoreMode, solution: Option<&str>) -> RestoreSettings {
    RestoreSettings {
        mode,
        solution: solution.map(str::to_string),
    }
}

#[test]
fn unlocked_restores_plainly_builds_and_opens_a_repo_without_lock_files() {
    let mut fake = Fake::nuget("1.2.0");
    fake.files.remove(LOCK);
    fake.original.remove(LOCK);
    let found = tree(&["Acme.sln", CSPROJ]);
    let applied = run_nuget_with(
        &mut fake,
        &found,
        &rows("1.2.0"),
        &settings(RestoreMode::Unlocked, None),
    )
    .unwrap();
    assert!(matches!(applied, Applied::Opened(_)), "{applied:?}");
    assert_eq!(
        fake.log,
        vec![
            "worktree".to_string(),
            format!("write {CSPROJ}"),
            "restore-plain Acme.sln".to_string(),
            "build Acme.sln".to_string(),
            "commit bump Acme.Util to 2.0.0".to_string(),
            "push pipetrain/bump/acme.util-2.0.0".to_string(),
            "open".to_string(),
        ]
    );
    assert_eq!(fake.committed, vec![CSPROJ.to_string()]);
}

#[test]
fn a_failing_build_stops_an_unlocked_bump_before_the_push() {
    let mut fake = Fake::nuget("1.2.0");
    fake.build_fails = true;
    let found = tree(&["Acme.sln", CSPROJ]);
    let error = run_nuget_with(
        &mut fake,
        &found,
        &rows("1.2.0"),
        &settings(RestoreMode::Unlocked, None),
    )
    .unwrap_err();
    assert!(error.to_string().contains("build failed"), "{error}");
    assert!(!fake.log.iter().any(|entry| entry.starts_with("push")));
}

#[test]
fn skip_pins_and_opens_without_restoring_or_building() {
    let mut fake = Fake::nuget("1.2.0");
    let found = tree(&[CSPROJ]);
    let applied = run_nuget_with(
        &mut fake,
        &found,
        &rows("1.2.0"),
        &settings(RestoreMode::Skip, None),
    )
    .unwrap();
    assert!(matches!(applied, Applied::Opened(_)), "{applied:?}");
    assert!(
        !fake
            .log
            .iter()
            .any(|entry| entry.starts_with("restore") || entry.starts_with("build")),
        "{:?}",
        fake.log
    );
}

#[test]
fn a_chosen_solution_resolves_a_repo_with_two_of_them() {
    let mut fake = Fake::nuget("1.2.0");
    let found = tree(&["A.sln", "B.slnx", CSPROJ, LOCK]);
    let applied = run_nuget_with(
        &mut fake,
        &found,
        &rows("1.2.0"),
        &settings(RestoreMode::Locked, Some("B.slnx")),
    )
    .unwrap();
    assert!(matches!(applied, Applied::Opened(_)), "{applied:?}");
    assert!(fake.log.contains(&"restore B.slnx locked=true".to_string()));
}

#[test]
fn two_solutions_without_a_choice_name_the_files_in_the_skip_reason() {
    let mut fake = Fake::nuget("1.2.0");
    let found = tree(&["A.sln", "B.slnx", CSPROJ, LOCK]);
    let Applied::Skipped(reason) = run_nuget(&mut fake, &found, &rows("1.2.0")).unwrap() else {
        panic!("expected a skip");
    };
    assert!(
        reason.contains("A.sln") && reason.contains("B.slnx"),
        "{reason}"
    );
    assert!(reason.contains("--solution"), "{reason}");
    assert!(fake.log.is_empty());
}

#[test]
fn nuget_pins_restores_twice_commits_manifest_and_lock_and_opens_the_pr() {
    let mut fake = Fake::nuget("1.2.0");
    let found = tree(&["Acme.sln", CSPROJ, LOCK]);
    let applied = run_nuget(&mut fake, &found, &rows("1.2.0")).unwrap();
    let Applied::Opened(opened) = applied else {
        panic!("expected a PR, got {applied:?}");
    };
    assert_eq!(opened.branch, "pipetrain/bump/acme.util-2.0.0");
    assert_eq!(opened.url, "https://github.com/acme/api/pull/9");
    assert_eq!(
        fake.log,
        vec![
            "worktree".to_string(),
            format!("write {CSPROJ}"),
            "restore Acme.sln locked=false".to_string(),
            "restore Acme.sln locked=true".to_string(),
            "commit bump Acme.Util to 2.0.0".to_string(),
            "push pipetrain/bump/acme.util-2.0.0".to_string(),
            "open".to_string(),
        ]
    );
    assert_eq!(fake.committed, vec![CSPROJ.to_string(), LOCK.to_string()]);
    assert!(fake.files[CSPROJ].contains("Version=\"2.0.0\""));
    assert!(
        fake.body
            .contains("pipetrain opened this draft pull request")
    );
    assert!(fake.body.contains("`Acme.Util` to 2.0.0"));
    assert!(fake.body.contains(&format!("`{CSPROJ}`: 1.2.0 -> 2.0.0")));
}

#[test]
fn nuget_without_lock_files_or_with_two_solutions_is_skipped_before_any_work() {
    for found in [
        tree(&["Acme.sln", CSPROJ]),
        tree(&["A.sln", "B.sln", CSPROJ, LOCK]),
        tree(&[CSPROJ, LOCK]),
    ] {
        let mut fake = Fake::nuget("1.2.0");
        let applied = run_nuget(&mut fake, &found, &rows("1.2.0")).unwrap();
        assert!(matches!(applied, Applied::Skipped(_)), "{found:?}");
        assert!(fake.log.is_empty(), "{:?}", fake.log);
    }
}

#[test]
fn nuget_that_already_pins_the_version_opens_nothing() {
    let mut fake = Fake::nuget("2.0.0");
    let found = tree(&["Acme.sln", CSPROJ, LOCK]);
    let applied = run_nuget(&mut fake, &found, &rows("1.2.0")).unwrap();
    assert!(matches!(applied, Applied::Skipped(_)));
    assert!(!fake.log.iter().any(|entry| entry.starts_with("push")));
    assert!(!fake.log.contains(&"open".to_string()));
}

#[test]
fn nuget_stops_when_the_restore_does_not_resolve_the_exact_version() {
    let mut fake = Fake::nuget("1.2.0");
    fake.resolves_lock = false;
    let found = tree(&["Acme.sln", CSPROJ, LOCK]);
    let error = run_nuget(&mut fake, &found, &rows("1.2.0")).unwrap_err();
    assert!(
        error.to_string().contains("restore changed files")
            || error.to_string().contains("did not produce"),
        "{error}"
    );
    assert!(!fake.log.iter().any(|entry| entry.starts_with("push")));
    assert!(!fake.log.contains(&"open".to_string()));
}

#[test]
fn a_moved_default_branch_stops_before_any_write() {
    let mut fake = Fake::nuget("1.2.0");
    fake.head = "f".repeat(40);
    let found = tree(&["Acme.sln", CSPROJ, LOCK]);
    let error = run_nuget(&mut fake, &found, &rows("1.2.0")).unwrap_err();
    assert!(error.to_string().contains("head moved"), "{error}");
    assert!(fake.log.is_empty());
}
