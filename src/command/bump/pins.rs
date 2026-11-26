use std::ops::Range;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;

use super::{Level, Status};
use crate::feed::Ecosystem;

mod go;
mod toml;
mod workspace;
pub use workspace::check_workspace_sources;

struct Pin {
    current: String,
    span: Option<Range<usize>>,
    redirected: bool,
}

impl Pin {
    fn manual(reason: &str) -> Self {
        Self {
            current: reason.to_owned(),
            span: None,
            redirected: false,
        }
    }

    fn alternate(reason: &str) -> Self {
        Self {
            redirected: true,
            ..Self::manual(reason)
        }
    }
}

pub(super) fn handles(ecosystem: Ecosystem) -> bool {
    matches!(
        ecosystem,
        Ecosystem::Cargo | Ecosystem::Pypi | Ecosystem::Go
    )
}

fn read(ecosystem: Ecosystem, text: &str, package: &str) -> Result<Vec<Pin>> {
    match ecosystem {
        Ecosystem::Go => Ok(go::pins(text, package)),
        Ecosystem::Cargo | Ecosystem::Pypi => toml::pins(ecosystem, text, package),
        _ => bail!("unexpected manifest ecosystem"),
    }
}

pub(crate) fn public_dependency(ecosystem: Ecosystem, text: &str, package: &str) -> bool {
    read(ecosystem, text, package)
        .is_ok_and(|pins| !pins.is_empty() && pins.iter().all(|pin| !pin.redirected))
}

pub(super) fn versions(ecosystem: Ecosystem, text: &str, package: &str) -> Result<Vec<String>> {
    Ok(read(ecosystem, text, package)?
        .into_iter()
        .map(|pin| pin.current)
        .collect())
}

fn version_part(ecosystem: Ecosystem, current: &str) -> Option<&str> {
    match ecosystem {
        Ecosystem::Cargo => current.strip_prefix('=').map(str::trim),
        Ecosystem::Pypi => Some(
            current
                .strip_prefix("==")
                .or_else(|| current.strip_prefix('='))
                .unwrap_or(current)
                .trim(),
        ),
        Ecosystem::Go => Some(current),
        _ => None,
    }
}

pub(super) fn valid_version(ecosystem: Ecosystem, package: &str, version: &str) -> bool {
    if version.trim() != version || version.is_empty() {
        return false;
    }
    match ecosystem {
        Ecosystem::Cargo => semver::Version::parse(version).is_ok_and(|v| v.build.is_empty()),
        Ecosystem::Pypi => version.parse::<pep440_rs::Version>().is_ok(),
        Ecosystem::Go => {
            static PSEUDO: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"(?:^|[.-])[0-9]{14}-[0-9a-f]{12}$").unwrap());
            let Some(number) = version
                .strip_prefix('v')
                .and_then(|v| semver::Version::parse(v).ok())
            else {
                return false;
            };
            if !number.build.is_empty() || PSEUDO.is_match(version) {
                return false;
            }
            let major_path = if package.starts_with("gopkg.in/") {
                package.rsplit_once(".v").map(|(_, n)| n)
            } else {
                package.rsplit_once("/v").map(|(_, n)| n)
            }
            .and_then(|s| s.parse::<u64>().ok());
            match major_path {
                Some(major) => major == number.major,
                None => number.major < 2,
            }
        }
        _ => false,
    }
}

pub(super) fn classify(ecosystem: Ecosystem, package: &str, current: &str, target: &str) -> Status {
    let Some(version) = version_part(ecosystem, current) else {
        return Status::Range;
    };
    if !valid_version(ecosystem, package, version) || !valid_version(ecosystem, package, target) {
        return Status::Range;
    }
    match ecosystem.compare_versions(version, target) {
        std::cmp::Ordering::Equal => Status::UpToDate,
        std::cmp::Ordering::Less => Status::Change,
        std::cmp::Ordering::Greater => Status::Ahead,
    }
}

pub(super) fn level(ecosystem: Ecosystem, current: &str, target: &str) -> Level {
    let current = version_part(ecosystem, current).unwrap_or(current);
    if ecosystem == Ecosystem::Pypi
        && let (Ok(from), Ok(to)) = (
            current.parse::<pep440_rs::Version>(),
            target.parse::<pep440_rs::Version>(),
        )
    {
        if from.epoch() != to.epoch() || from.release().first() != to.release().first() {
            return Level::Major;
        }
        return if from.release().get(1).unwrap_or(&0) == to.release().get(1).unwrap_or(&0) {
            Level::Patch
        } else {
            Level::Minor
        };
    }
    super::level_of(current, target)
}

pub fn pin_manifest(
    ecosystem: Ecosystem,
    text: &str,
    package: &str,
    target: &str,
    expected: &[String],
) -> Result<String> {
    let mut pins = read(ecosystem, text, package)?;
    pins.retain(|pin| {
        expected.contains(&pin.current)
            && classify(ecosystem, package, &pin.current, target) == Status::Change
    });
    let mut actual: Vec<_> = pins.iter().map(|pin| &pin.current).collect();
    let mut wanted: Vec<_> = expected.iter().collect();
    actual.sort();
    wanted.sort();
    if actual.is_empty() || actual != wanted {
        bail!("the exact references to {package} have changed since the dry run; run bump again");
    }
    let mut spans: Vec<_> = pins
        .into_iter()
        .map(|pin| {
            pin.span
                .ok_or_else(|| anyhow::anyhow!("cannot safely edit this reference to {package}"))
        })
        .collect::<Result<_>>()?;
    spans.sort_by_key(|s| s.start);
    let mut out = text.to_string();
    for span in spans.into_iter().rev() {
        out.replace_range(span, target);
    }
    Ok(out)
}
