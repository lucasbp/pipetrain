use serde_json::Value;

use super::error::UpdateError;
use super::version::Version;
use crate::github::origin;

pub const REPO: &str = "lucasbp/pipetrain";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn parse(body: &Value) -> Result<Self, UpdateError> {
        let tag = body
            .get("tag_name")
            .and_then(Value::as_str)
            .ok_or(UpdateError::NoRelease)?
            .to_string();
        let parsed =
            Version::parse(&tag).ok_or_else(|| UpdateError::BadTag { tag: tag.clone() })?;
        if parsed.is_prerelease() {
            return Err(UpdateError::BadTag { tag });
        }
        let assets = body
            .get("assets")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|item| {
                        Some(Asset {
                            name: item.get("name")?.as_str()?.to_string(),
                            url: item.get("browser_download_url")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
        Ok(Self {
            tag,
            version,
            assets,
        })
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

pub fn is_trusted(base: &str, url: &str) -> bool {
    let host = origin(url).to_ascii_lowercase();
    host == "https://github.com"
        || host == "https://api.github.com"
        || host == origin(base).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_the_tag_and_the_assets() {
        let body = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "a.zip", "browser_download_url": "https://github.com/x/a.zip"},
                {"name": "broken"}
            ]
        });
        let release = Release::parse(&body).unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.assets.len(), 1);
        assert!(release.asset("a.zip").is_some());
        assert!(release.asset("b.zip").is_none());
    }

    #[test]
    fn a_body_without_a_tag_is_no_release() {
        assert_eq!(
            Release::parse(&json!({"message": "Not Found"})),
            Err(UpdateError::NoRelease)
        );
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_refused() {
        let error = Release::parse(&json!({"tag_name": "nightly"})).unwrap_err();
        assert!(matches!(error, UpdateError::BadTag { .. }), "{error:?}");
        let error = Release::parse(&json!({"tag_name": "v1.0.0-rc.1"})).unwrap_err();
        assert!(matches!(error, UpdateError::BadTag { .. }), "{error:?}");
    }

    #[test]
    fn only_github_and_the_api_base_are_trusted() {
        let base = "http://127.0.0.1:4000";
        assert!(is_trusted(
            base,
            "https://github.com/lucasbp/pipetrain/releases/download/v1/a"
        ));
        assert!(is_trusted(base, "http://127.0.0.1:4000/dl/a"));
        assert!(!is_trusted(base, "https://evil.example/a"));
        assert!(!is_trusted(base, "http://github.com/a"));
        assert!(!is_trusted(base, "https://github.com.evil.example/a"));
    }
}
