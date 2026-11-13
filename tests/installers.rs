use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pipetrain-installers-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(windows)]
mod common;

#[cfg(windows)]
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
mod powershell {
    use super::*;
    use std::io::{Cursor, Write};
    use std::net::TcpListener;
    use std::thread;

    use pipetrain::update::checksum::sha256_hex;

    fn server(bad_checksum: bool) -> String {
        let mut archive = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut archive);
        zip.start_file("pipetrain.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"new executable").unwrap();
        zip.finish().unwrap();
        let archive = archive.into_inner();
        let hash = if bad_checksum {
            "0".repeat(64)
        } else {
            sha256_hex(&archive)
        };
        let sums = format!("{hash}  pipetrain-v99.0.0-x86_64-pc-windows-msvc.zip\n");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let location = format!("{base}/tag/v99.0.0");
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let request = common::read_head(&mut stream);
                let first = request.lines().next().unwrap();
                let (status, headers, body): (_, _, &[u8]) = if first.contains("/latest ") {
                    ("302 Found", format!("Location: {location}\r\n"), b"")
                } else if first.contains("/SHA256SUMS ") {
                    ("200 OK", String::new(), sums.as_bytes())
                } else {
                    ("200 OK", String::new(), &archive)
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        base
    }

    fn install(sandbox: &Sandbox, failure: &str, bad_checksum: bool) -> Output {
        let install_dir = sandbox.0.join("installed");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("pipetrain.exe"), "old executable").unwrap();
        let wrapper = sandbox.0.join("run.ps1");
        fs::write(
            &wrapper,
            r#"$ErrorActionPreference = 'Stop'
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing, [int]$TimeoutSec)
    if ($TimeoutSec -ne 120) { throw 'missing download deadline' }
    Add-Content -LiteralPath $env:INSTALL_TEST_LOG -Value $Uri
    Microsoft.PowerShell.Utility\Invoke-WebRequest @PSBoundParameters
}
function Move-Item {
    param($Path, $Destination, $LiteralPath, [switch]$Force)
    $source = if ($LiteralPath) { $LiteralPath } else { $Path }
    if ($env:INSTALL_TEST_FAILURE -eq 'promote' -and $source.EndsWith('.new')) {
        throw 'injected promotion failure'
    }
    Microsoft.PowerShell.Management\Move-Item -LiteralPath $source -Destination $Destination -Force:$Force
}
function Write-Host {
    param([Parameter(ValueFromRemainingArguments=$true)]$Message)
    if ($env:INSTALL_TEST_FAILURE -eq 'final' -and "$Message".StartsWith('Installed pipetrain')) {
        throw 'injected final step failure'
    }
}
& $env:INSTALL_TEST_SCRIPT -Version latest -InstallDir $env:INSTALL_TEST_DIR -ReleaseBase $env:INSTALL_TEST_BASE
if ($LASTEXITCODE) { exit $LASTEXITCODE }
"#,
        )
        .unwrap();
        Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(wrapper)
            .env(
                "INSTALL_TEST_SCRIPT",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("install.ps1"),
            )
            .env("INSTALL_TEST_DIR", install_dir)
            .env("INSTALL_TEST_BASE", server(bad_checksum))
            .env("INSTALL_TEST_FAILURE", failure)
            .env("INSTALL_TEST_LOG", sandbox.0.join("downloads"))
            .env("PROCESSOR_ARCHITECTURE", "AMD64")
            .env_remove("PROCESSOR_ARCHITEW6432")
            .output()
            .unwrap()
    }

    #[test]
    fn powershell_downloads_have_deadlines_and_verified_install_succeeds() {
        let sandbox = Sandbox::new();
        assert_success(&install(&sandbox, "", false));
        assert_eq!(
            fs::read(sandbox.0.join("installed/pipetrain.exe")).unwrap(),
            b"new executable"
        );
        assert_eq!(
            fs::read_to_string(sandbox.0.join("downloads"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn powershell_failed_promotion_restores_the_previous_binary() {
        let sandbox = Sandbox::new();
        assert!(!install(&sandbox, "promote", false).status.success());
        assert_eq!(
            fs::read(sandbox.0.join("installed/pipetrain.exe")).unwrap(),
            b"old executable"
        );
        assert!(!sandbox.0.join("installed/pipetrain.exe.new").exists());
    }

    #[test]
    fn powershell_failed_final_step_rolls_back_the_promoted_binary() {
        let sandbox = Sandbox::new();
        assert!(!install(&sandbox, "final", false).status.success());
        assert_eq!(
            fs::read(sandbox.0.join("installed/pipetrain.exe")).unwrap(),
            b"old executable"
        );
    }

    #[test]
    fn powershell_checksum_failure_preserves_the_previous_binary() {
        let sandbox = Sandbox::new();
        assert!(!install(&sandbox, "", true).status.success());
        assert_eq!(
            fs::read(sandbox.0.join("installed/pipetrain.exe")).unwrap(),
            b"old executable"
        );
        assert!(!sandbox.0.join("installed/pipetrain.exe.old").exists());
    }
}

fn shell() -> Command {
    #[cfg(windows)]
    let program = "C:/Program Files/Git/bin/bash.exe";
    #[cfg(not(windows))]
    let program = "sh";
    Command::new(program)
}

fn shell_test(mode: &str) -> Output {
    let sandbox = Sandbox::new();
    let script = sandbox.0.join("test.sh");
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh")).unwrap();
    let wrapper = r#"set -eu
command() {
  if [ "$1" = -v ]; then
    if [ "$2" = curl ] && [ "$MODE" != curl ]; then return 1; fi
    if [ "$2" = wget ]; then printf 'wget\n'; return 0; fi
  fi
  /bin/sh -c 'command -v "$1"' sh "$2"
}
uname() { if [ "$1" = -s ]; then printf 'Linux\n'; else printf 'x86_64\n'; fi; }
curl() {
  case " $* " in *' --connect-timeout 15 '*) ;; *) exit 81 ;; esac
  case " $* " in *' --max-time 120 '*) ;; *) exit 82 ;; esac
  case " $* " in *'/latest'*) printf 'http://127.0.0.1/tag/v99.0.0'; return 0 ;; esac
  printf 'curl deadline checked\n'
  return 1
}
wget() {
  case " $* " in *' --connect-timeout=15 '*) ;; *) exit 83 ;; esac
  case " $* " in *' --timeout=120 '*) ;; *) exit 84 ;; esac
  case " $* " in *' --tries=1 '*) ;; *) exit 85 ;; esac
  if [ "$MODE" = watchdog ]; then
    exec /bin/sleep 20
  fi
  printf 'wget deadline checked\n'
  return 1
}
sleep() { /bin/sleep 0.05; }
"#;
    fs::write(&script, format!("{wrapper}\n{source}")).unwrap();
    shell()
        .arg(script.to_string_lossy().replace('\\', "/"))
        .env("MODE", mode)
        .env("PIPETRAIN_RELEASE_BASE", "http://127.0.0.1/releases")
        .env(
            "PIPETRAIN_VERSION",
            if mode == "curl" { "latest" } else { "99.0.0" },
        )
        .env("PIPETRAIN_INSTALL_DIR", sandbox.0.join("installed"))
        .output()
        .unwrap()
}

#[test]
fn shell_curl_latest_and_download_requests_have_deadlines() {
    let output = shell_test("curl");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("curl deadline checked"));
}

#[test]
fn shell_wget_download_has_deadlines_and_one_attempt() {
    let output = shell_test("wget");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("wget deadline checked"));
}

#[test]
fn shell_wget_total_deadline_stops_a_continuous_download() {
    let start = std::time::Instant::now();
    let output = shell_test("watchdog");
    assert!(!output.status.success());
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not download"));
}
