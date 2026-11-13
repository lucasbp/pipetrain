mod common;

use anyhow::{Result, bail};
use pipetrain::config::ApiConfig;
use pipetrain::consumer_bump::backend::{ConsumerRemote, Invocation, LocalBackend, Runner};
use pipetrain::consumer_bump::{self, Consumer};
use pipetrain::github::Client;
use pipetrain::publication::backend::collect_run;
use pipetrain::publication::{self, ExactFeed, PublisherExecution, PublisherRun};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

const SHA: &str = "1111111111111111111111111111111111111111";
fn execution() -> PublisherExecution {
    PublisherExecution {
        repository: "acme/library".into(),
        merge_sha: SHA.into(),
        run_id: 42,
        published_this_execution: true,
    }
}
struct Feed;
impl ExactFeed for Feed {
    fn contains(&mut self, _: &str, version: &str) -> Result<bool> {
        Ok(version == "1.2.3.4")
    }
}
#[test]
fn github_collection_ignores_skipped_auxiliary_job_and_outside_step_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let serving = std::thread::spawn(move || {
        let run = serde_json::json!({"id":42,"repository":{"full_name":"acme/library"},"head_sha":SHA,"status":"completed","conclusion":"success"}).to_string();
        let jobs = serde_json::json!({"total_count":2,"jobs":[{"id":1,"conclusion":"skipped","steps":[]},{"id":2,"conclusion":"success","steps":[{"name":"Push","conclusion":"success","started_at":"2026-01-01T12:00:00Z","completed_at":"2026-01-01T12:00:02Z"}]}]}).to_string();
        let log = "2026-01-01T11:00:00Z Pushing Acme.Util.9.9.9.nupkg to 'feed'...\n2026-01-01T11:00:01Z Your package was pushed.\n2026-01-01T12:00:00.1Z Pushing Acme.Util.1.2.3.4.nupkg to 'feed'...\n2026-01-01T12:00:01Z Your package was pushed.\n";
        for body in [run.as_str(), jobs.as_str(), log] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                socket.read_exact(&mut b).unwrap();
                request.push(b[0]);
            }
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    });
    let mut client = Client::with_base_url(
        "ghp_FAKE0123456789".into(),
        &ApiConfig::default(),
        false,
        &base,
    );
    let run = collect_run(&mut client, &execution(), "Push", &base).unwrap();
    let result = publication::verify(&execution(), &run, "Acme.Util", &mut Feed).unwrap();
    assert!(matches!(
        result,
        publication::PublicationResult::Published { .. }
    ));
    serving.join().unwrap();
}

struct Remote;
impl ConsumerRemote for Remote {
    fn reserve_branch(&mut self, repository: &str, branch: &str, source_sha: &str) -> Result<()> {
        assert_eq!(repository, "acme/api");
        assert!(branch.starts_with("pipetrain/bump/"));
        assert_eq!(source_sha, SHA);
        Ok(())
    }
    fn head(&mut self, _: &Consumer) -> Result<String> {
        Ok(SHA.into())
    }
    fn open(&mut self, c: &Consumer, _: &str, _: &str) -> Result<String> {
        assert_eq!(c.head_branch, "feature/consumer");
        Ok("https://github.com/acme/api/pull/8".into())
    }
}
#[derive(Default)]
struct Commands {
    calls: Vec<Invocation>,
    tree: Option<PathBuf>,
    fail_restore: bool,
}
impl Runner for Commands {
    fn run(&mut self, command: &Invocation) -> Result<String> {
        self.calls.push(command.clone());
        let has = |arg: &str| command.args.iter().any(|s| s == arg);
        if has("init") {
            fs::create_dir(command.directory.join("repository.git"))?;
        }
        if has("worktree") {
            let tree = command.directory.parent().unwrap().join("worktree");
            fs::create_dir_all(tree.join("src"))?;
            fs::write(tree.join("src/App.sln"), "solution fixture")?;
            fs::write(
                tree.join("src/App.csproj"),
                "<Project><PackageReference Include=\"Acme.Util\" Version=\"1.0.0\" /></Project>",
            )?;
            self.tree = Some(tree);
        }
        if command.program == "dotnet" {
            assert!(has("--use-lock-file"));
            if self.fail_restore {
                bail!("simulated restore failure");
            }
            fs::write(
                command.directory.join("src/packages.lock.json"),
                r#"{"dependencies":{"net8.0":{"Acme.Util":{"resolved":"1.2.3.4"}}}}"#,
            )?;
        }
        if has("rev-parse") {
            return Ok(SHA.into());
        }
        if has("status")
            && self
                .tree
                .as_ref()
                .unwrap()
                .join("src/packages.lock.json")
                .exists()
        {
            return Ok(" M src/App.csproj\n?? src/packages.lock.json".into());
        }
        Ok(String::new())
    }
}
fn consumer() -> Consumer {
    Consumer {
        repository: "acme/api".into(),
        number: 7,
        head_branch: "feature/consumer".into(),
        head_sha: SHA.into(),
        default_branch: "main".into(),
        bump_branch: "pipetrain/bump/unique-42".into(),
        solution: "src/App.sln".into(),
        manifests: vec!["src/App.csproj".into()],
    }
}
fn root() -> common::TempDir {
    common::TempDir::new("bump-fixture")
}
fn proof() -> publication::PublicationResult {
    publication::verify(
        &execution(),
        &PublisherRun {
            repository: "acme/library".into(),
            head_sha: SHA.into(),
            id: 42,
            succeeded: true,
            push_step_succeeded: true,
            push_step_log: "Pushing Acme.Util.1.2.3.4.nupkg to 'feed'...\nYour package was pushed."
                .into(),
        },
        "Acme.Util",
        &mut Feed,
    )
    .unwrap()
}
#[test]
fn concrete_backend_builds_shell_free_commands_and_preserves_exact_locked_graph() {
    let root = root();
    let mut backend = LocalBackend::new(
        root.path().to_path_buf(),
        Commands::default(),
        Remote,
        Duration::from_secs(30),
    );
    consumer_bump::prepare(&proof(), &consumer(), &mut backend).unwrap();
    let commands = &backend.runner.calls;
    assert_eq!(commands.iter().filter(|c| c.program == "dotnet").count(), 2);
    assert_eq!(
        commands
            .iter()
            .filter(|c| c.args.iter().any(|a| a == "--locked-mode"))
            .count(),
        1
    );
    assert!(
        commands
            .iter()
            .all(|c| c.program == "git" || c.program == "dotnet")
    );
    assert!(
        commands
            .iter()
            .all(|c| !c.args.iter().any(|a| a.starts_with("--force")))
    );
    let push = commands
        .iter()
        .find(|c| c.args.iter().any(|a| a == "push"))
        .unwrap();
    assert!(
        push.args
            .contains(&"HEAD:refs/heads/pipetrain/bump/unique-42".into())
    );
    assert!(
        backend
            .workspace()
            .unwrap()
            .join("worktree/src/packages.lock.json")
            .exists()
    );
}
#[test]
fn restore_failure_retains_new_workspace_without_pushing_or_touching_existing_files() {
    let root = root();
    fs::write(root.join("existing-work.txt"), "keep").unwrap();
    let mut backend = LocalBackend::new(
        root.path().to_path_buf(),
        Commands {
            fail_restore: true,
            ..Commands::default()
        },
        Remote,
        Duration::from_secs(30),
    );
    assert!(consumer_bump::prepare(&proof(), &consumer(), &mut backend).is_err());
    assert!(
        backend
            .workspace()
            .unwrap()
            .join("worktree/src/App.csproj")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(root.join("existing-work.txt")).unwrap(),
        "keep"
    );
    assert!(
        !backend
            .runner
            .calls
            .iter()
            .any(|c| c.args.iter().any(|a| a == "commit" || a == "push"))
    );
}
