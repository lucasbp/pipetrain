use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::command::apply::tint;
use crate::config::ApiConfig;
use crate::github::{self, ApiError, Client, Download, origin};
use crate::progress::{self, Progress};
use crate::style::{bold, dim, green};
use crate::update::display::readable_path;
use crate::update::release::{REPO, Release, is_trusted};
use crate::update::target::{self, CHECKSUMS_ASSET};
use crate::update::{UpdateError, Version, archive, checksum, replace};

const API: &str = "https://api.github.com";
const SUMS_LIMIT: u64 = 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 64 * 1024 * 1024;
const NEWER: i32 = 10;

pub struct UpdateOptions {
    pub check: bool,
    pub dry_run: bool,
    pub json: bool,
    pub progress: bool,
    pub api_url: Option<String>,
    pub verbose: bool,
}

struct Installed {
    asset: String,
    sha256: Option<String>,
    executable: PathBuf,
}

fn host_of(base: &str) -> String {
    origin(base)
        .split_once("://")
        .map_or(base, |(_, host)| host)
        .to_string()
}

fn api_error(error: ApiError, base: &str) -> UpdateError {
    match error {
        ApiError::Network => UpdateError::Network {
            host: host_of(base),
        },
        other => UpdateError::Api {
            message: other.to_string(),
        },
    }
}

fn latest(client: &mut Client, base: &str) -> Result<Release, UpdateError> {
    let path = format!("/repos/{REPO}/releases/latest");
    let reply = client.get(&path).map_err(|error| api_error(error, base))?;
    match reply.into_body(&path) {
        Ok(body) => Release::parse(&body),
        Err(ApiError::NotFound { .. }) => Err(UpdateError::NoRelease),
        Err(error) => Err(api_error(error, base)),
    }
}

fn fetch_release(base: &str, verbose: bool) -> Result<(Client, Release), UpdateError> {
    let token = github::lookup()
        .ok()
        .map(github::Lookup::into_token)
        .unwrap_or_default();
    let cfg = ApiConfig::default();
    let mut client = Client::with_base_url(token.clone(), &cfg, verbose, base);
    match latest(&mut client, base) {
        Err(UpdateError::Api { .. }) if !token.is_empty() => {
            let mut anonymous = Client::with_base_url(String::new(), &cfg, verbose, base);
            let release = latest(&mut anonymous, base)?;
            Ok((anonymous, release))
        }
        Ok(release) => Ok((client, release)),
        Err(error) => Err(error),
    }
}

fn download(
    client: &mut Client,
    base: &str,
    url: &str,
    what: &str,
    limit: u64,
) -> Result<Vec<u8>, UpdateError> {
    if !is_trusted(base, url) {
        return Err(UpdateError::UntrustedUrl {
            url: url.to_string(),
        });
    }
    client.download(url, limit).map_err(|error| match error {
        Download::TooLarge => UpdateError::TooLarge {
            what: what.to_string(),
            limit,
        },
        Download::Api(error) => api_error(error, base),
    })
}

fn download_verified(
    client: &mut Client,
    base: &str,
    release: &Release,
    name: &str,
) -> Result<(Vec<u8>, String), UpdateError> {
    let asset = release.asset(name).ok_or_else(|| UpdateError::NoAsset {
        name: name.to_string(),
        tag: release.tag.clone(),
    })?;
    let sums = release
        .asset(CHECKSUMS_ASSET)
        .ok_or_else(|| UpdateError::NoChecksums {
            tag: release.tag.clone(),
        })?;
    progress::phase("downloading the checksums");
    let text = download(client, base, &sums.url, CHECKSUMS_ASSET, SUMS_LIMIT)?;
    let expected = checksum::lookup(&String::from_utf8_lossy(&text), name).ok_or_else(|| {
        UpdateError::ChecksumMissing {
            asset: name.to_string(),
        }
    })?;
    progress::phase(&format!("downloading {name}"));
    let bytes = download(client, base, &asset.url, name, ARCHIVE_LIMIT)?;
    progress::phase("verifying the checksum");
    let actual = checksum::sha256_hex(&bytes);
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch {
            asset: name.to_string(),
        });
    }
    Ok((bytes, actual))
}

fn running_executable() -> Result<PathBuf, UpdateError> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| UpdateError::Replace {
            path: "the running executable".to_string(),
            reason: error.to_string(),
        })
}

fn asset_for(release: &Release) -> Result<(target::Target, String), UpdateError> {
    let target = target::current()?;
    let name = target.asset_name(&release.version);
    Ok((target, name))
}

fn install(
    client: &mut Client,
    base: &str,
    release: &Release,
    executable: PathBuf,
) -> Result<Installed, UpdateError> {
    let (target, name) = asset_for(release)?;
    replace::check_writable(&executable)?;
    let (bytes, sha256) = download_verified(client, base, release, &name)?;
    let binary = archive::extract(target.format, &bytes, target.exe, ARCHIVE_LIMIT)?;
    progress::phase("replacing the executable");
    replace::discard_leftover(&executable);
    replace::replace(&executable, &binary)?;
    Ok(Installed {
        asset: name,
        sha256: Some(sha256),
        executable,
    })
}

const SHORT_SHA: usize = 12;

#[derive(Clone, Copy)]
struct Look {
    color: bool,
    verbose: bool,
}

fn installed_text(current: &str, version: &str, done: &Installed, look: Look) -> String {
    let headline = format!(
        "{} {}",
        tint(look.color, bold, &format!("pipetrain {current} ->")),
        tint(look.color, green, version)
    );
    let label = |name: &str| tint(look.color, dim, &format!("{name:<8}"));
    let mut lines = vec![headline, format!("{} {}", label("asset"), done.asset)];
    if let Some(sha256) = &done.sha256 {
        let shown = if look.verbose {
            sha256.as_str()
        } else {
            sha256.get(..SHORT_SHA).unwrap_or(sha256)
        };
        lines.push(format!("{} {shown}", label("sha256")));
    }
    lines.push(format!(
        "{} {}",
        label("location"),
        readable_path(&done.executable)
    ));
    lines.push(tint(look.color, green, "ok"));
    lines.join("\n")
}

fn up_to_date_text(current: &str, color: bool) -> String {
    tint(color, green, &format!("pipetrain {current} is up to date"))
}

fn print_report(
    json: bool,
    current: &str,
    release: &Release,
    newer: bool,
    dry_run: bool,
    installed: Option<&Installed>,
    look: Look,
) -> Result<()> {
    if json {
        let report = json!({
            "schema": 1,
            "current": current,
            "latest": release.version,
            "tag": release.tag,
            "update_available": newer,
            "updated": installed.is_some() && !dry_run,
            "dry_run": dry_run,
            "asset": installed.map(|done| done.asset.clone()),
            "sha256": installed.and_then(|done| done.sha256.clone()),
            "executable": installed.map(|done| done.executable.display().to_string()),
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if let Some(done) = installed.filter(|_| dry_run) {
        println!("dry run: pipetrain {current} -> {}", release.version);
        println!("asset {}", done.asset);
        println!("would replace {}", readable_path(&done.executable));
        println!("nothing was downloaded or written");
    } else if let Some(done) = installed {
        anstream::println!("{}", installed_text(current, &release.version, done, look));
    } else if newer {
        println!("update available: {current} -> {}", release.version);
        println!("run `pipetrain update` to install it");
    } else {
        anstream::println!("{}", up_to_date_text(current, look.color));
    }
    Ok(())
}

pub fn run(opts: UpdateOptions) -> Result<i32> {
    let base = opts.api_url.as_deref().unwrap_or(API).to_string();
    let current = env!("CARGO_PKG_VERSION");
    let progress = Progress::start(opts.progress);
    progress::phase("checking the latest release");
    let (mut client, release) = fetch_release(&base, opts.verbose)?;
    let newer = Version::parse(&release.version) > Version::parse(current);
    let installed = if newer && opts.dry_run {
        let (_, asset) = asset_for(&release)?;
        Some(Installed {
            asset,
            sha256: None,
            executable: running_executable()?,
        })
    } else if newer && !opts.check {
        Some(install(
            &mut client,
            &base,
            &release,
            running_executable()?,
        )?)
    } else {
        None
    };
    drop(progress);
    print_report(
        opts.json,
        current,
        &release,
        newer,
        opts.dry_run,
        installed.as_ref(),
        Look {
            color: !opts.check,
            verbose: opts.verbose,
        },
    )?;
    Ok(if newer && opts.check { NEWER } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "0a48e125abcdef0123456789abcdef0123456789abcdef0123456789abcdef01";

    fn installed() -> Installed {
        Installed {
            asset: "pipetrain-v0.10.0-x86_64-pc-windows-msvc.zip".to_string(),
            sha256: Some(FULL.to_string()),
            executable: PathBuf::from(r"\\?\D:\Portable\pipetrain\pipetrain.exe"),
        }
    }

    fn plain(verbose: bool) -> Look {
        Look {
            color: false,
            verbose,
        }
    }

    #[test]
    fn the_plain_report_is_short_aligned_and_has_no_escapes() {
        let text = installed_text("0.9.2", "0.10.0", &installed(), plain(false));
        assert_eq!(
            text,
            "pipetrain 0.9.2 -> 0.10.0\n\
             asset    pipetrain-v0.10.0-x86_64-pc-windows-msvc.zip\n\
             sha256   0a48e125abcd\n\
             location D:\\Portable\\pipetrain\\pipetrain.exe\n\
             ok"
        );
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn verbose_prints_the_full_hash() {
        let text = installed_text("0.9.2", "0.10.0", &installed(), plain(true));
        assert!(text.contains(&format!("sha256   {FULL}")), "{text}");
    }

    #[test]
    fn the_colored_report_bolds_the_headline_and_greens_the_new_version() {
        let look = Look {
            color: true,
            verbose: false,
        };
        let text = installed_text("0.9.2", "0.10.0", &installed(), look);
        let first = text.lines().next().unwrap();
        assert_eq!(
            first,
            "\x1b[1mpipetrain 0.9.2 ->\x1b[0m \x1b[32m0.10.0\x1b[0m"
        );
        assert!(text.ends_with("\x1b[32mok\x1b[0m"), "{text}");
    }

    #[test]
    fn up_to_date_is_one_green_line_only_when_colored() {
        assert_eq!(
            up_to_date_text("0.10.0", false),
            "pipetrain 0.10.0 is up to date"
        );
        assert_eq!(
            up_to_date_text("0.10.0", true),
            "\x1b[32mpipetrain 0.10.0 is up to date\x1b[0m"
        );
    }
}
