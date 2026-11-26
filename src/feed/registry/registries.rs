use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::{Ecosystem, Fetched, Outcome, Session};

mod retractions;
pub(crate) use retractions::exclude_go_retractions;

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

pub(crate) fn validate_version(ecosystem: Ecosystem, version: Option<&str>) -> Result<()> {
    let Some(version) = version else {
        return Ok(());
    };
    match ecosystem {
        Ecosystem::Cargo => {
            semver::Version::parse(version)
                .context("Cargo feed checks require an exact major.minor.patch version")?;
        }
        Ecosystem::Pypi => {
            version
                .parse::<pep440_rs::Version>()
                .context("PyPI feed checks require an exact PEP 440 version")?;
        }
        Ecosystem::Go => {
            semver::Version::parse(version.strip_prefix('v').unwrap_or(version))
                .context("Go feed checks require an exact module version")?;
        }
        _ => {}
    }
    Ok(())
}

fn go_versions(
    session: &Session,
    base: &str,
    package: &str,
    version: Option<&str>,
) -> Result<Outcome<Vec<String>>> {
    let module = go_path(package)?;
    let root = format!("{base}/{module}");
    let requested = version.map(|v| {
        if v.starts_with('v') {
            v.to_string()
        } else {
            format!("v{v}")
        }
    });
    let (mut versions, listed) =
        match session.get_body(Ecosystem::Go, &format!("{root}/@v/list"), |body| {
            let text = body.read_to_string()?;
            let mut versions = Vec::new();
            for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
                go_version(line)?;
                versions.push(line.to_string());
            }
            Ok(versions)
        })? {
            Outcome::Body(versions) => (versions, true),
            Outcome::NotFound => (Vec::new(), false),
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
                    version
                        .is_none_or(|v| Ecosystem::Go.compare_versions(&info.version, v).is_eq()),
                    "Go proxy returned a different version"
                );
                versions.push(info.version);
            }
            Outcome::NotFound if !listed => return Ok(Outcome::NotFound),
            Outcome::NotFound => {}
            Outcome::Refused { had_token } => return Ok(Outcome::Refused { had_token }),
        }
    }
    Ok(Outcome::Body(versions))
}

pub(super) fn fetch(
    session: &Session,
    ecosystem: Ecosystem,
    package: &str,
    version: Option<&str>,
    registry: Option<&str>,
) -> Result<Fetched> {
    let default = match ecosystem {
        Ecosystem::Cargo => "https://index.crates.io",
        Ecosystem::Pypi => "https://pypi.org",
        Ecosystem::Go => "https://proxy.golang.org",
        _ => unreachable!(),
    };
    let base = registry.unwrap_or(default).trim_end_matches('/');
    let response = match ecosystem {
        Ecosystem::Cargo => {
            let path = cargo_path(package)?;
            session.get_body(ecosystem, &format!("{base}/{path}"), |body| {
                cargo_versions(package, &body.read_to_string()?)
            })?
        }
        Ecosystem::Pypi => {
            let name = python_name(package)?;
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
        Ecosystem::Go => go_versions(session, base, package, version)?,
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
