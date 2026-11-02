use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use pipetrain::update::checksum::sha256_hex;
use pipetrain::update::target::{self, Format};
use serde_json::{Value, json};

const FAKE_TOKEN: &str = "ghp_FAKEupdate0123456789";
const NEW_BYTES: &[u8] = b"NEW-EXECUTABLE-BYTES";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn reply(status: u16, body: &[u8]) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: body.to_vec(),
    }
}

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn lines(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
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

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if read == 0 || text.contains("\r\n\r\n") {
            return text;
        }
    }
}

fn serve(make: impl FnOnce(String) -> Box<dyn Fn(&str) -> Reply + Send>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handler = make(base.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read_request(&mut stream);
            let line = request
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end_matches(" HTTP/1.1")
                .to_string();
            let answer = handler(&line);
            record.lock().unwrap().push(request);
            let mut head = format!("HTTP/1.1 {} X\r\n", answer.status);
            for (name, value) in &answer.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                answer.body.len()
            ));
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&answer.body);
        }
    });
    Server { base, seen }
}

fn archive_with(name: &str, body: &[u8]) -> Vec<u8> {
    match target::current().unwrap().format {
        Format::Zip => {
            let mut out = Cursor::new(Vec::new());
            let mut writer = zip::ZipWriter::new(&mut out);
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
            writer.finish().unwrap();
            out.into_inner()
        }
        Format::TarGz => {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, body).unwrap();
            let tar = builder.into_inner().unwrap();
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(&tar).unwrap();
            gz.finish().unwrap()
        }
    }
}

#[derive(Clone)]
struct Scenario {
    tag: String,
    release: bool,
    with_asset: bool,
    with_sums: bool,
    sums: Option<String>,
    archive: Vec<u8>,
    asset_url: Option<String>,
}

impl Scenario {
    fn new() -> Self {
        Self {
            tag: "v99.0.0".to_string(),
            release: true,
            with_asset: true,
            with_sums: true,
            sums: None,
            archive: archive_with(target::current().unwrap().exe, NEW_BYTES),
            asset_url: None,
        }
    }

    fn asset_name(&self) -> String {
        let version = self.tag.trim_start_matches('v');
        target::current().unwrap().asset_name(version)
    }

    fn release_json(&self, base: &str) -> Value {
        let mut assets = Vec::new();
        if self.with_asset {
            let url = self
                .asset_url
                .clone()
                .unwrap_or_else(|| format!("{base}/dl/archive"));
            assets.push(json!({"name": self.asset_name(), "browser_download_url": url}));
        }
        if self.with_sums {
            assets.push(json!({
                "name": "SHA256SUMS",
                "browser_download_url": format!("{base}/dl/sums")
            }));
        }
        json!({"tag_name": self.tag, "assets": assets})
    }

    fn sums_text(&self) -> String {
        self.sums
            .clone()
            .unwrap_or_else(|| format!("{}  {}\n", sha256_hex(&self.archive), self.asset_name()))
    }

    fn start(&self) -> Server {
        let scenario = self.clone();
        serve(move |base| {
            Box::new(move |line| match line {
                "GET /repos/lucasbp/pipetrain/releases/latest" if scenario.release => {
                    reply(200, scenario.release_json(&base).to_string().as_bytes())
                }
                "GET /repos/lucasbp/pipetrain/releases/latest" => {
                    reply(404, br#"{"message":"Not Found"}"#)
                }
                "GET /dl/sums" => reply(200, scenario.sums_text().as_bytes()),
                "GET /dl/archive" => Reply {
                    status: 302,
                    headers: vec![("Location".to_string(), format!("{base}/dl/blob"))],
                    body: Vec::new(),
                },
                "GET /dl/blob" => reply(200, &scenario.archive),
                _ => reply(404, b"{}"),
            })
        })
    }
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Sandbox {
    dir: PathBuf,
    exe: PathBuf,
    original: Vec<u8>,
}

impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "pipetrain-update-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = PathBuf::from(env!("CARGO_BIN_EXE_pipetrain"));
        let exe = dir.join(source.file_name().unwrap());
        fs::copy(&source, &exe).unwrap();
        let original = fs::read(&exe).unwrap();
        Self { dir, exe, original }
    }

    fn run(&self, server: &Server, args: &[&str]) -> Output {
        Command::new(&self.exe)
            .arg("update")
            .args(["--api-url", &server.base, "--no-progress"])
            .args(args)
            .env("PATH", &self.dir)
            .env("GH_TOKEN", FAKE_TOKEN)
            .output()
            .unwrap()
    }

    fn untouched(&self) -> bool {
        fs::read(&self.exe).unwrap() == self.original
    }

    fn leftovers(&self) -> bool {
        fs::read_dir(&self.dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".new"))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn failed(scenario: &Scenario, expect: &str) -> (Sandbox, Server, String) {
    let sandbox = Sandbox::new();
    let server = scenario.start();
    let out = sandbox.run(&server, &["--experimental-install"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(expect), "{stderr}");
    assert!(sandbox.untouched(), "the executable changed");
    assert!(!sandbox.leftovers());
    assert!(!stderr.contains(FAKE_TOKEN) && !text(&out.stdout).contains(FAKE_TOKEN));
    (sandbox, server, stderr)
}

#[test]
fn check_with_a_newer_release_exits_10_and_downloads_nothing() {
    let sandbox = Sandbox::new();
    let server = Scenario::new().start();
    let out = sandbox.run(&server, &["--check"]);
    assert_eq!(out.status.code(), Some(10));
    assert!(text(&out.stdout).contains("update available"));
    assert!(text(&out.stdout).contains("99.0.0"));
    assert_eq!(
        server.lines(),
        vec!["GET /repos/lucasbp/pipetrain/releases/latest"]
    );
    assert!(sandbox.untouched());
}

#[test]
fn check_with_the_same_version_exits_0() {
    let mut scenario = Scenario::new();
    scenario.tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let sandbox = Sandbox::new();
    let server = scenario.start();
    let out = sandbox.run(&server, &["--check"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).contains("up to date"));
}

#[test]
fn an_older_release_is_not_installed() {
    let mut scenario = Scenario::new();
    scenario.tag = "v0.0.1".to_string();
    let sandbox = Sandbox::new();
    let server = scenario.start();
    let out = sandbox.run(&server, &["--experimental-install"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(server.lines().len(), 1);
    assert!(sandbox.untouched());
}

#[test]
fn update_replaces_the_executable_after_verifying_it() {
    let sandbox = Sandbox::new();
    let server = Scenario::new().start();
    let out = sandbox.run(&server, &["--experimental-install"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&sandbox.exe).unwrap(), NEW_BYTES);
    assert!(!sandbox.leftovers());
    let stdout = text(&out.stdout);
    assert!(stdout.contains("updated pipetrain"), "{stdout}");
    assert!(!stdout.contains(FAKE_TOKEN) && !stderr.contains(FAKE_TOKEN));
    let first = server.requests().remove(0).to_lowercase();
    assert!(first.contains(&format!(
        "authorization: bearer {}",
        FAKE_TOKEN.to_lowercase()
    )));
    assert!(first.contains("user-agent: pipetrain/"));
    let lines = server.lines();
    assert!(lines.contains(&"GET /dl/blob".to_string()), "{lines:?}");
    assert!(lines.iter().all(|line| line.starts_with("GET ")));
}

#[test]
fn json_reports_the_update() {
    let sandbox = Sandbox::new();
    let server = Scenario::new().start();
    let out = sandbox.run(&server, &["--experimental-install", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["latest"], "99.0.0");
    assert_eq!(report["update_available"], true);
    assert_eq!(report["updated"], true);
    assert_eq!(report["sha256"], sha256_hex(&Scenario::new().archive));
}

#[test]
fn json_check_reports_without_updating() {
    let sandbox = Sandbox::new();
    let server = Scenario::new().start();
    let out = sandbox.run(&server, &["--check", "--json"]);
    assert_eq!(out.status.code(), Some(10));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["update_available"], true);
    assert_eq!(report["updated"], false);
    assert!(report["asset"].is_null());
}

#[test]
fn a_wrong_checksum_changes_nothing() {
    let mut scenario = Scenario::new();
    scenario.sums = Some(format!("{}  {}\n", "0".repeat(64), scenario.asset_name()));
    failed(&scenario, "SHA-256");
}

#[test]
fn a_checksum_list_without_the_asset_changes_nothing() {
    let mut scenario = Scenario::new();
    scenario.sums = Some(format!("{}  other.zip\n", "a".repeat(64)));
    failed(&scenario, "does not list");
}

#[test]
fn a_release_without_checksums_changes_nothing() {
    let mut scenario = Scenario::new();
    scenario.with_sums = false;
    let (_, server, _) = failed(&scenario, "no SHA256SUMS");
    assert_eq!(server.lines().len(), 1);
}

#[test]
fn a_release_without_the_platform_asset_changes_nothing() {
    let mut scenario = Scenario::new();
    scenario.with_asset = false;
    failed(&scenario, "has no asset named");
}

#[test]
fn an_archive_without_the_executable_changes_nothing() {
    let mut scenario = Scenario::new();
    scenario.archive = archive_with("README.md", b"hello");
    failed(&scenario, "not usable");
}

#[test]
fn an_asset_outside_github_is_never_fetched() {
    let mut scenario = Scenario::new();
    scenario.asset_url = Some("https://evil.invalid/pipetrain.zip".to_string());
    let (_, server, _) = failed(&scenario, "not a GitHub download address");
    assert!(
        server
            .lines()
            .iter()
            .all(|line| !line.contains("evil") && line != "GET /dl/blob")
    );
}

#[test]
fn a_tag_that_is_not_a_version_changes_nothing() {
    let mut scenario = Scenario::new();
    scenario.tag = "nightly".to_string();
    failed(&scenario, "not a version");
}

#[test]
fn update_without_a_flag_requests_nothing_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let server = Scenario::new().start();
    let out = sandbox.run(&server, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("not ready yet"), "{stderr}");
    assert!(stderr.contains("install.sh") && stderr.contains("install.ps1"));
    assert!(server.lines().is_empty(), "{:?}", server.lines());
    assert!(sandbox.untouched());
    assert!(!sandbox.leftovers());
}

#[test]
fn the_install_flag_cannot_be_combined_with_check() {
    let sandbox = Sandbox::new();
    let server = Scenario::new().start();
    let out = sandbox.run(&server, &["--check", "--experimental-install"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(server.lines().is_empty());
    assert!(sandbox.untouched());
}

#[test]
fn no_release_is_a_clear_error() {
    let mut scenario = Scenario::new();
    scenario.release = false;
    failed(&scenario, "no release is published");
}
