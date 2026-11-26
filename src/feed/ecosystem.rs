use std::cmp::Ordering;

use super::{Ecosystem, compare, is_prerelease};

impl Ecosystem {
    pub fn compare_versions(self, left: &str, right: &str) -> Ordering {
        match self {
            Self::Cargo | Self::Go => {
                let parse = |s: &str| semver::Version::parse(self.semver_text(s));
                match (parse(left), parse(right)) {
                    (Ok(a), Ok(b)) => a.cmp_precedence(&b).then_with(|| a.cmp(&b)),
                    _ => left.cmp(right),
                }
            }
            Self::Pypi => match (
                left.parse::<pep440_rs::Version>(),
                right.parse::<pep440_rs::Version>(),
            ) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => left.cmp(right),
            },
            Self::Nuget | Self::Npm => compare(left, right),
        }
    }

    pub fn is_prerelease(self, version: &str) -> bool {
        match self {
            Self::Pypi => version
                .parse::<pep440_rs::Version>()
                .is_ok_and(|v| v.any_prerelease()),
            Self::Cargo | Self::Go => {
                semver::Version::parse(self.semver_text(version)).is_ok_and(|v| !v.pre.is_empty())
            }
            Self::Nuget | Self::Npm => is_prerelease(version),
        }
    }

    fn semver_text(self, version: &str) -> &str {
        if self == Self::Go {
            version.strip_prefix('v').unwrap_or(version)
        } else {
            version
        }
    }
}
