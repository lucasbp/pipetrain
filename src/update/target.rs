use super::error::UpdateError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    TarGz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub triple: &'static str,
    pub format: Format,
    pub exe: &'static str,
}

const WINDOWS_X64: Target = Target {
    triple: "x86_64-pc-windows-msvc",
    format: Format::Zip,
    exe: "pipetrain.exe",
};
const LINUX_X64: Target = Target {
    triple: "x86_64-unknown-linux-musl",
    format: Format::TarGz,
    exe: "pipetrain",
};
const LINUX_ARM64: Target = Target {
    triple: "aarch64-unknown-linux-musl",
    format: Format::TarGz,
    exe: "pipetrain",
};
const MAC_ARM64: Target = Target {
    triple: "aarch64-apple-darwin",
    format: Format::TarGz,
    exe: "pipetrain",
};
const MAC_X64: Target = Target {
    triple: "x86_64-apple-darwin",
    format: Format::TarGz,
    exe: "pipetrain",
};

pub fn for_platform(os: &str, arch: &str) -> Option<Target> {
    match (os, arch) {
        ("windows", "x86_64") => Some(WINDOWS_X64),
        ("linux", "x86_64") => Some(LINUX_X64),
        ("linux", "aarch64") => Some(LINUX_ARM64),
        ("macos", "aarch64") => Some(MAC_ARM64),
        ("macos", "x86_64") => Some(MAC_X64),
        _ => None,
    }
}

pub fn current() -> Result<Target, UpdateError> {
    for_platform(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        UpdateError::UnsupportedPlatform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    })
}

impl Target {
    pub fn asset_name(&self, version: &str) -> String {
        let extension = match self.format {
            Format::Zip => "zip",
            Format::TarGz => "tar.gz",
        };
        format!("pipetrain-v{version}-{}.{extension}", self.triple)
    }
}

pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_platform_has_a_distinct_asset() {
        let names: Vec<String> = [
            ("windows", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
        ]
        .iter()
        .map(|(os, arch)| for_platform(os, arch).unwrap().asset_name("1.2.3"))
        .collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
        assert_eq!(names[0], "pipetrain-v1.2.3-x86_64-pc-windows-msvc.zip");
        assert_eq!(
            names[1],
            "pipetrain-v1.2.3-x86_64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn an_unknown_platform_has_no_target() {
        assert!(for_platform("freebsd", "x86_64").is_none());
        assert!(for_platform("windows", "aarch64").is_none());
    }
}
