use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::{Ecosystem, FeedOptions, Fetched, Outcome, Session};

#[derive(Deserialize)]
struct CrateRelease {
    name: String,
    vers: String,
    yanked: bool,
}

fn cargo_path(name: &str) -> Result<String> {
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
        "invalid crate name"
    );
    let name = name.to_ascii_lowercase();
    let prefix = match name.len() {
        1 | 2 => name.len().to_string(),
        3 => format!("3/{}", &name[..1]),
        _ => format!("{}/{}", &name[..2], &name[2..4]),
    };
    Ok(format!("{prefix}/{name}"))
}

fn cargo_versions(name: &str, text: &str) -> Result<Vec<String>> {
    let mut versions = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let release: CrateRelease =
            serde_json::from_str(line).context("invalid Cargo sparse index record")?;
        ensure!(
            release.name.eq_ignore_ascii_case(name),
            "Cargo index returned a different crate"
        );
        semver::Version::parse(&release.vers).context("invalid Cargo release version")?;
        if !release.yanked {
            versions.push(release.vers);
        }
    }
    Ok(versions)
}

#[derive(Deserialize)]
struct PythonFile {
    yanked: bool,
}

#[derive(Deserialize)]
struct PythonProject {
    releases: BTreeMap<String, Vec<PythonFile>>,
}

fn python_name(package: &str) -> Result<String> {
    ensure!(
        package.starts_with(|c: char| c.is_ascii_alphanumeric())
            && package.ends_with(|c: char| c.is_ascii_alphanumeric())
            && package
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')),
        "invalid PyPI project name"
    );
    Ok(package
        .to_ascii_lowercase()
        .split(['.', '_', '-'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-"))
}

fn go_escape(value: &str) -> String {
    let mut path = String::new();
    for c in value.chars() {
        if c.is_ascii_uppercase() {
            path.push('!');
            path.push(c.to_ascii_lowercase());
        } else {
            path.push(c);
        }
    }
    path
}

fn go_path(package: &str) -> Result<String> {
    ensure!(
        package.split('/').all(|part| !part.is_empty()
            && !part.starts_with('.')
            && !part.ends_with('.')
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~'))),
        "invalid Go module path"
    );
    Ok(go_escape(package))
}

fn go_version(text: &str) -> Result<()> {
    ensure!(
        text.starts_with('v') && semver::Version::parse(&text[1..]).is_ok(),
        "invalid Go module version"
    );
    Ok(())
}

#[derive(Deserialize)]
struct GoInfo {
    #[serde(rename = "Version")]
    version: String,
}

fn go_versions(session: &Session, base: &str, opts: &FeedOptions) -> Result<Outcome<Vec<String>>> {
    let module = go_path(&opts.package)?;
    let root = format!("{base}/{module}");
    let requested = opts.version.as_deref().map(|v| {
        if v.starts_with('v') {
            v.to_string()
        } else {
            format!("v{v}")
        }
    });
    if let Some(v) = &requested {
        go_version(v)?;
    }
    let mut versions =
        match session.get_body(Ecosystem::Go, &format!("{root}/@v/list"), |body| {
            let text = body.read_to_string()?;
            let mut versions = Vec::new();
            for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
                go_version(line)?;
                versions.push(line.to_string());
            }
            Ok(versions)
        })? {
            Outcome::Body(versions) => versions,
            Outcome::NotFound if requested.is_none() => return Ok(Outcome::NotFound),
            Outcome::NotFound => Vec::new(),
            Outcome::Refused { had_token } => return Ok(Outcome::Refused { had_token }),
        };
    let endpoint = match requested {
        Some(v) if !versions.contains(&v) => Some(format!("@v/{}.info", go_escape(&v))),
        None if versions.is_empty() => Some("@latest".to_string()),
        _ => None,
    };
    if let Some(endpoint) = endpoint {
        match session.get_body(Ecosystem::Go, &format!("{root}/{endpoint}"), |body| {
            Ok(body.read_json::<GoInfo>()?)
        })? {
            Outcome::Body(info) => {
                go_version(&info.version)?;
                ensure!(
                    opts.version
                        .as_deref()
                        .is_none_or(|v| Ecosystem::Go.compare_versions(&info.version, v).is_eq()),
                    "Go proxy returned a different version"
                );
                versions.push(info.version);
            }
            Outcome::NotFound => {}
            Outcome::Refused { had_token } => return Ok(Outcome::Refused { had_token }),
        }
    }
    Ok(Outcome::Body(versions))
}

pub(super) fn fetch(
    session: &Session,
    ecosystem: Ecosystem,
    opts: &FeedOptions,
) -> Result<Fetched> {
    let default = match ecosystem {
        Ecosystem::Cargo => "https://index.crates.io",
        Ecosystem::Pypi => "https://pypi.org",
        Ecosystem::Go => "https://proxy.golang.org",
        _ => unreachable!(),
    };
    let base = opts
        .registry
        .as_deref()
        .unwrap_or(default)
        .trim_end_matches('/');
    let response = match ecosystem {
        Ecosystem::Cargo => {
            let path = cargo_path(&opts.package)?;
            session.get_body(ecosystem, &format!("{base}/{path}"), |body| {
                cargo_versions(&opts.package, &body.read_to_string()?)
            })?
        }
        Ecosystem::Pypi => {
            let name = python_name(&opts.package)?;
            session.get_body(ecosystem, &format!("{base}/pypi/{name}/json"), |body| {
                let project: PythonProject =
                    body.read_json().context("invalid PyPI project metadata")?;
                Ok(project
                    .releases
                    .into_iter()
                    .filter(|(v, files)| {
                        v.parse::<pep440_rs::Version>().is_ok()
                            && files.iter().any(|file| !file.yanked)
                    })
                    .map(|(v, _)| v)
                    .collect())
            })?
        }
        Ecosystem::Go => go_versions(session, base, opts)?,
        _ => unreachable!(),
    };
    Ok(match response {
        Outcome::Body(versions) => Fetched::Found {
            feed: base.into(),
            versions,
        },
        Outcome::NotFound => Fetched::NotFound { feed: base.into() },
        Outcome::Refused { had_token } => Fetched::Refused { had_token },
    })
}
