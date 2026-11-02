use std::fmt;

use crate::hint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Network { host: String },
    Api { message: String },
    NoRelease,
    BadTag { tag: String },
    UnsupportedPlatform { os: String, arch: String },
    NoAsset { name: String, tag: String },
    NoChecksums { tag: String },
    ChecksumMissing { asset: String },
    ChecksumMismatch { asset: String },
    UntrustedUrl { url: String },
    TooLarge { what: String, limit: u64 },
    BadArchive { reason: String },
    Replace { path: String, reason: String },
}

const MANUAL: &str = "Download the release by hand from https://github.com/lucasbp/pipetrain/releases and check it against SHA256SUMS, or run the install script again.";

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network { host } => write!(
                f,
                "cannot reach {host}, nothing was changed. {}",
                hint::connectivity_fix()
            ),
            Self::Api { message } => write!(f, "{message} Nothing was changed."),
            Self::NoRelease => write!(
                f,
                "no release is published yet, nothing was changed. Build from source or wait for the first release."
            ),
            Self::BadTag { tag } => write!(
                f,
                "the latest release is tagged \"{tag}\", which is not a version, nothing was changed. {MANUAL}"
            ),
            Self::UnsupportedPlatform { os, arch } => write!(
                f,
                "no prebuilt binary for {os} {arch}, nothing was changed. Build from source with `cargo install --git https://github.com/lucasbp/pipetrain`."
            ),
            Self::NoAsset { name, tag } => write!(
                f,
                "release {tag} has no asset named {name}, nothing was changed. The release may still be uploading: try again in a few minutes, or {MANUAL}"
            ),
            Self::NoChecksums { tag } => write!(
                f,
                "release {tag} has no SHA256SUMS, so the download cannot be verified and nothing was changed. {MANUAL}"
            ),
            Self::ChecksumMissing { asset } => write!(
                f,
                "SHA256SUMS does not list {asset}, so the download cannot be verified and nothing was changed. {MANUAL}"
            ),
            Self::ChecksumMismatch { asset } => write!(
                f,
                "the SHA-256 of {asset} does not match SHA256SUMS, so the download was discarded and nothing was changed. Try again; if it keeps failing, do not install it and open an issue."
            ),
            Self::UntrustedUrl { url } => write!(
                f,
                "the release points to {url}, which is not a GitHub download address, so nothing was fetched and nothing was changed. {MANUAL}"
            ),
            Self::TooLarge { what, limit } => write!(
                f,
                "{what} is larger than {limit} bytes, so it was refused and nothing was changed. {MANUAL}"
            ),
            Self::BadArchive { reason } => write!(
                f,
                "the downloaded archive is not usable ({reason}), nothing was changed. Try again, or {MANUAL}"
            ),
            Self::Replace { path, reason } => write!(
                f,
                "could not replace {path}: {reason}. The old executable is still in place. Close other running pipetrain processes, or move it to a folder you can write to and run `pipetrain update` again."
            ),
        }
    }
}

impl std::error::Error for UpdateError {}
