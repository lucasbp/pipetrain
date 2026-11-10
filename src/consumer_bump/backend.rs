use super::{BumpGateway, Consumer};
use crate::github::{BranchWrite, Client};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub directory: PathBuf,
    pub timeout: Duration,
}

pub trait Runner {
    fn run(&mut self, invocation: &Invocation) -> Result<String>;
}

pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&mut self, invocation: &Invocation) -> Result<String> {
        if invocation.timeout.is_zero() || invocation.timeout > Duration::from_secs(3600) {
            bail!("bump subprocess timeout must be positive and at most one hour");
        }
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .current_dir(&invocation.directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .context("cannot start isolated bump operation")?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let drain = |mut input: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut kept = Vec::new();
                let mut chunk = [0; 8192];
                loop {
                    let count = input.read(&mut chunk)?;
                    if count == 0 {
                        break;
                    }
                    let available = (1024 * 1024_usize).saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..count.min(available)]);
                }
                Ok::<_, std::io::Error>(kept)
            })
        };
        let out = drain(Box::new(stdout));
        let err = drain(Box::new(stderr));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= invocation.timeout {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let _ = Command::new("taskkill")
                        .args(["/PID", &child.id().to_string(), "/T", "/F"])
                        .creation_flags(0x08000000)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                #[cfg(unix)]
                {
                    let _ = Command::new("kill")
                        .args(["-KILL", &format!("-{}", child.id())])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                let _ = child.kill();
                let _ = child.wait();
                bail!("isolated bump operation timed out; inspect the preserved workspace");
            }
            thread::sleep(Duration::from_millis(50));
        };
        while !out.is_finished() || !err.is_finished() {
            if started.elapsed() >= invocation.timeout {
                let _ = child.kill();
                bail!(
                    "bump output pipes did not close within the operation timeout; workspace preserved"
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
        let bytes = out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader failed"))??;
        let _ = err
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader failed"))??;
        if !status.success() {
            bail!(
                "isolated {} operation failed ({status}); workspace preserved",
                invocation.program
            );
        }
        Ok(String::from_utf8(bytes)
            .context("operation output was not UTF-8")?
            .trim_end()
            .to_string())
    }
}

pub trait ConsumerRemote {
    fn head(&mut self, consumer: &Consumer) -> Result<String>;
    fn reserve_branch(&mut self, repository: &str, branch: &str, source_sha: &str) -> Result<()>;
    fn open(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String>;
}

pub struct GithubRemote<'a> {
    pub client: &'a mut Client,
}

impl ConsumerRemote for GithubRemote<'_> {
    fn reserve_branch(&mut self, repository: &str, branch: &str, source_sha: &str) -> Result<()> {
        if self.client.create_branch(repository, branch, source_sha)? != BranchWrite::Created {
            bail!("bump branch already exists; refusing to update it");
        }
        Ok(())
    }
    fn head(&mut self, consumer: &Consumer) -> Result<String> {
        let pr = self.client.rest(&format!(
            "/repos/{}/pulls/{}",
            consumer.repository, consumer.number
        ))?;
        if pr["state"] != "open"
            || pr["head"]["ref"] != consumer.head_branch
            || pr["head"]["repo"]["full_name"]
                .as_str()
                .is_none_or(|r| !r.eq_ignore_ascii_case(&consumer.repository))
        {
            bail!("consumer PR must remain open in its own repository and expected head branch");
        }
        Ok(pr["head"]["sha"]
            .as_str()
            .context("missing consumer head SHA")?
            .to_string())
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
                "title": "Update published package dependency", "body": body,
                "head": consumer.bump_branch, "base": consumer.head_branch, "draft": true,
            }),
        )?;
        if reply.status != 201 {
            bail!("GitHub did not create the bump review PR");
        }
        Ok(reply.body["html_url"]
            .as_str()
            .context("missing bump PR URL")?
            .to_string())
    }
}

pub struct LocalBackend<R, G> {
    pub runner: R,
    pub remote: G,
    root: PathBuf,
    slot: Option<PathBuf>,
    tree: Option<PathBuf>,
    repository: Option<String>,
    source_sha: Option<String>,
    timeout: Duration,
}

impl<R: Runner, G: ConsumerRemote> LocalBackend<R, G> {
    pub fn new(root: PathBuf, runner: R, remote: G, timeout: Duration) -> Self {
        Self {
            root,
            runner,
            remote,
            slot: None,
            tree: None,
            repository: None,
            source_sha: None,
            timeout,
        }
    }
    pub fn workspace(&self) -> Option<&Path> {
        self.slot.as_deref()
    }
    fn execute(&mut self, directory: &Path, program: &str, args: &[String]) -> Result<String> {
        let mut safe_args = Vec::new();
        if program == "git" {
            safe_args.extend([
                "-c".into(),
                "core.hooksPath=".into(),
                "-c".into(),
                "core.fsmonitor=false".into(),
            ]);
        }
        safe_args.extend_from_slice(args);
        self.runner.run(&Invocation {
            program: program.into(),
            args: safe_args,
            directory: directory.into(),
            timeout: self.timeout,
        })
    }
    fn git(&mut self, args: &[&str]) -> Result<String> {
        let tree = self.tree.clone().context("isolated worktree not created")?;
        self.execute(
            &tree,
            "git",
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }
    fn file(&self, path: &str) -> Result<PathBuf> {
        if !super::safe_path(path) {
            bail!("unsafe workspace path");
        }
        let tree = self
            .tree
            .as_ref()
            .context("isolated worktree not created")?
            .canonicalize()?;
        let file = tree
            .join(path)
            .canonicalize()
            .context("workspace file must exist")?;
        if !file.starts_with(&tree) || !file.is_file() {
            bail!("workspace path escapes isolated worktree");
        }
        Ok(file)
    }
}

impl<R: Runner, G: ConsumerRemote> BumpGateway for LocalBackend<R, G> {
    fn consumer_head(&mut self, consumer: &Consumer) -> Result<String> {
        self.remote.head(consumer)
    }
    fn create_worktree(&mut self, consumer: &Consumer) -> Result<()> {
        let parts: Vec<_> = consumer.repository.split('/').collect();
        if self.slot.is_some()
            || parts.len() != 2
            || parts.iter().any(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            })
            || consumer.head_branch.starts_with('-')
            || consumer.bump_branch.starts_with('-')
        {
            bail!("invalid repository or reused bump workspace");
        }
        fs::create_dir_all(&self.root)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let slot = self
            .root
            .join(format!("bump-{}-{stamp}", std::process::id()));
        fs::create_dir(&slot)?;
        fs::File::create(slot.join("pipetrain-owned"))?.write_all(b"isolated bump workspace\n")?;
        self.slot = Some(slot.clone());
        self.execute(
            &slot,
            "git",
            &["init".into(), "--bare".into(), "repository.git".into()],
        )?;
        let bare = slot.join("repository.git");
        let url = format!("https://github.com/{}.git", consumer.repository);
        for branch in [&consumer.head_branch, &consumer.bump_branch] {
            self.execute(
                &bare,
                "git",
                &[
                    "check-ref-format".into(),
                    "--branch".into(),
                    branch.to_string(),
                ],
            )?;
        }
        self.execute(
            &bare,
            "git",
            &[
                "fetch".into(),
                "--depth=1".into(),
                url,
                format!("refs/heads/{}", consumer.head_branch),
            ],
        )?;
        let actual = self.execute(&bare, "git", &["rev-parse".into(), "FETCH_HEAD".into()])?;
        if actual != consumer.head_sha {
            bail!("fetched consumer head moved; workspace preserved");
        }
        self.execute(
            &bare,
            "git",
            &[
                "worktree".into(),
                "add".into(),
                "-b".into(),
                consumer.bump_branch.clone(),
                "../worktree".into(),
                actual,
            ],
        )?;
        self.tree = Some(slot.join("worktree"));
        self.repository = Some(consumer.repository.clone());
        self.source_sha = Some(consumer.head_sha.clone());
        Ok(())
    }
    fn read(&mut self, path: &str) -> Result<String> {
        Ok(fs::read_to_string(self.file(path)?)?)
    }
    fn write(&mut self, path: &str, text: &str) -> Result<()> {
        Ok(fs::write(self.file(path)?, text)?)
    }
    fn restore(&mut self, solution: &str, locked: bool) -> Result<()> {
        self.file(solution)?;
        let tree = self.tree.clone().context("missing worktree")?;
        let mut args = vec![
            "restore".into(),
            solution.into(),
            "--nologo".into(),
            "--disable-build-servers".into(),
            "--use-lock-file".into(),
        ];
        if locked {
            args.push("--locked-mode".into());
        }
        self.execute(&tree, "dotnet", &args)?;
        Ok(())
    }
    fn changed_files(&mut self) -> Result<Vec<String>> {
        let status = self.git(&["status", "--porcelain=v1", "--untracked-files=all"])?;
        status
            .lines()
            .map(|line| {
                if line.len() < 4
                    || line.starts_with('R')
                    || line.starts_with('C')
                    || line[3..].contains(" -> ")
                    || line[3..].starts_with('"')
                {
                    bail!("unsupported workspace status");
                }
                Ok(line[3..].to_string())
            })
            .collect()
    }
    fn commit(&mut self, paths: &[String], message: &str) -> Result<String> {
        let mut args = vec!["add", "--"];
        args.extend(paths.iter().map(String::as_str));
        self.git(&args)?;
        self.git(&["-c", "core.hooksPath=", "commit", "-m", message])?;
        self.git(&["rev-parse", "HEAD"])
    }
    fn push_new_branch(&mut self, branch: &str, commit: &str) -> Result<()> {
        if self.git(&["rev-parse", "HEAD"])? != commit {
            bail!("bump commit moved");
        }
        let repo = self.repository.clone().context("missing repository")?;
        let url = format!("https://github.com/{repo}.git");
        let reference = format!("refs/heads/{branch}");
        self.remote.reserve_branch(
            &repo,
            branch,
            self.source_sha.as_deref().context("missing source SHA")?,
        )?;
        self.git(&[
            "-c",
            "core.hooksPath=",
            "push",
            &url,
            &format!("HEAD:{reference}"),
        ])?;
        Ok(())
    }
    fn open_review_pr(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String> {
        self.remote.open(consumer, commit, body)
    }
}
