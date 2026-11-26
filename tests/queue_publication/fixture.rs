use super::api::Github;
use crate::common::git::{file_url, git_in};
use crate::common::{Server, TempDir, serve_raw};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub const REVIEW_URL: &str = "https://github.com/acme/web/pull/99";
const PROJECT: &str = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net8.0</TargetFramework><IsPackable>false</IsPackable></PropertyGroup><ItemGroup><PackageReference Include="Acme.Core" Version="1.0.0" /></ItemGroup></Project>"#;
const CONFIG: &str = "orgs = ['acme']\n[api]\nmax_requests = 2000\nmax_per_second = 1000\nmax_per_minute = 10000\nmin_remaining = 0\n[merge]\npackage_max_behind = 1\npoll_seconds = 1\ntimeout_minutes = 1\nno_deploy_repos = ['acme/web','acme/other']\n";

fn dotnet_env(command: &mut Command, dir: &TempDir) {
    command
        .env("DOTNET_CLI_HOME", dir.join("dotnet-home"))
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE", "true")
        .env("DOTNET_NOLOGO", "1")
        .env("NUGET_PACKAGES", dir.join("packages"))
        .env("NUGET_HTTP_CACHE_PATH", dir.join("nuget-cache"));
}

fn local_packages(dir: &TempDir, checkout: &Path) {
    let feed = dir.join("local-feed");
    fs::create_dir(&feed).unwrap();
    for version in ["1.0.0", "1.1.0"] {
        let file = fs::File::create(feed.join(format!("Acme.Core.{version}.nupkg"))).unwrap();
        let mut package = zip::ZipWriter::new(file);
        package
            .start_file("Acme.Core.nuspec", zip::write::SimpleFileOptions::default())
            .unwrap();
        write!(package, r#"<package><metadata><id>Acme.Core</id><version>{version}</version><authors>Test</authors><description>Local test</description></metadata></package>"#).unwrap();
        package.finish().unwrap();
    }
    fs::write(checkout.join("NuGet.Config"),format!(r#"<configuration><packageSources><clear/><add key="test" value="{}"/></packageSources></configuration>"#,feed.display())).unwrap();
    fs::write(
        checkout.join("Web.slnx"),
        r#"<Solution><Project Path="Web.csproj" /></Solution>"#,
    )
    .unwrap();
    let mut command = Command::new("dotnet");
    dotnet_env(&mut command, dir);
    let output = command
        .args(["restore", "Web.slnx", "--use-lock-file"])
        .current_dir(checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

pub struct Fixture {
    pub dir: TempDir,
    pub gh: Arc<Mutex<Github>>,
    pub server: Server,
    locked: bool,
}

impl Fixture {
    pub fn new(with_evidence: bool, locked: bool) -> Self {
        let dir = TempDir::new("queue-publication");
        fs::write(dir.join("pipetrain.toml"), CONFIG).unwrap();
        let remotes = dir.join("remotes");
        let repo = remotes.join("acme/web.git");
        let checkout = dir.join("web");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir(&checkout).unwrap();
        git_in(&repo, &["init", "--bare", "-b", "main"]);
        git_in(&repo, &["config", "core.logAllRefUpdates", "always"]);
        git_in(&checkout, &["init", "-b", "main"]);
        fs::write(checkout.join(".gitignore"), "bin/\nobj/\n").unwrap();
        fs::write(checkout.join("Web.csproj"), PROJECT).unwrap();
        if locked {
            local_packages(&dir, &checkout);
        }
        git_in(&checkout, &["add", "."]);
        git_in(
            &checkout,
            &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
        );
        for number in [2, 3] {
            let branch = format!("feature/{number}");
            git_in(&checkout, &["switch", "-c", &branch, "main"]);
            let path = format!("Feature{number}.cs");
            fs::write(checkout.join(&path), format!("class Feature{number} {{}}")).unwrap();
            git_in(&checkout, &["add", &path]);
            git_in(
                &checkout,
                &["-c", "commit.gpgsign=false", "commit", "-m", &branch],
            );
        }
        git_in(&checkout, &["push", "--all", repo.to_str().unwrap()]);
        fs::write(dir.join("gitconfig"),format!("[url \"{}\"]\n insteadOf = https://github.com/\n[user]\n name = Test\n email = test@example.com\n[commit]\n gpgsign = false\n",file_url(&remotes))).unwrap();
        let gh = Arc::new(Mutex::new(Github::new(repo, with_evidence)));
        let shared = gh.clone();
        let server = serve_raw(move |line, body, request| {
            let mut gh = shared.lock().unwrap();
            let host = request
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("host").then(|| value.trim())
                })
                .unwrap();
            let (status, body) = match line {
                "GET /nuget/index.json" => (200,json!({"resources":[{"@id":format!("http://{host}/flat/"),"@type":"PackageBaseAddress/3.0.0"},{"@id":format!("http://{host}/packages/acme/"),"@type":"PackagePublish/2.0.0"}]}).to_string()),
                "GET /flat/acme.core/index.json" => {
                    let mut versions = vec!["1.0.0"];
                    if gh.merged.contains(&1) {
                        versions.push(if gh.feed_has_version { "1.1.0" } else { "1.2.0" });
                    }
                    (200, json!({"versions":versions}).to_string())
                }
                "GET /repos/acme/sdk/actions/jobs/80/logs" => (200, gh.push_log(host)),
                _ => { let (code,value) = gh.route(line, body); (code,value.to_string()) },
            };
            (status, Vec::new(), body)
        });
        Self {
            dir,
            gh,
            server,
            locked,
        }
    }

    pub fn merge(&self, extra: &[&str]) -> String {
        self.merge_expecting(extra, &[0, 5])
    }

    pub fn merge_expecting(&self, extra: &[&str], codes: &[i32]) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
        dotnet_env(&mut command, &self.dir);
        let output = command
            .args([
                "merge",
                "acme/sdk#1",
                "acme/web#2",
                "acme/web#3",
                "acme/other#4",
                "--bump",
                "--no-progress",
                "--dependency-failure",
                "skip",
                "--restore-mode",
                if self.locked { "locked" } else { "skip" },
                "--state",
                "queue.json",
            ])
            .args(extra)
            .current_dir(self.dir.path())
            .stdin(Stdio::null())
            .env("PIPETRAIN_API_URL", &self.server.base)
            .env(
                "PIPETRAIN_NUGET_INDEX_URL",
                format!("{}/nuget/index.json", self.server.base),
            )
            .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
            .env("PIPETRAIN_LOG", "0")
            .env("PIPETRAIN_CACHE_DIR", self.dir.join("cache"))
            .env("GH_TOKEN", "ghp_FAKE0123456789")
            .env_remove("GITHUB_TOKEN")
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env_remove("PIPETRAIL_SEQUENCE_GIT_BASE")
            .env("HTTPS_PROXY", &self.server.base)
            .env("HTTP_PROXY", &self.server.base)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            output
                .status
                .code()
                .is_some_and(|code| codes.contains(&code)),
            "exit {:?}\n{}\n{text}\n{:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            self.gh.lock().unwrap().unexpected
        );
        text
    }

    pub fn report(&self) -> Value {
        let text = self.merge(&["--apply", "--yes", "--json"]);
        serde_json::from_str(&text).unwrap()
    }
}
