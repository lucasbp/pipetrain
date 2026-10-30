use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{ChangedFile, PrKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dep {
    Pr(PrKey),
    Invalid(String),
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

pub fn depends_on(body: &str, own_slug: &str) -> Vec<Dep> {
    static COMMENTS: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    static SHORT: OnceLock<Regex> = OnceLock::new();
    let stripped = regex(&COMMENTS, r"(?s)<!--.*?-->").replace_all(body, "");
    let line = regex(&LINE, r"(?im)^[ \t>*-]*depends-on:[ \t]*(.+)$");
    let short = regex(&SHORT, r"^#(\d+)$");
    let mut found = Vec::new();
    for caps in line.captures_iter(&stripped) {
        for raw in caps[1].split(|c: char| c == ',' || c.is_whitespace()) {
            let token = raw.trim_matches(|c: char| ".,;`*()<>".contains(c));
            if token.is_empty() {
                continue;
            }
            if let Some(short_caps) = short.captures(token) {
                let key = format!("{own_slug}#{}", &short_caps[1]);
                match PrKey::parse(&key) {
                    Some(k) => found.push(Dep::Pr(k)),
                    None => found.push(Dep::Invalid(token.to_string())),
                }
            } else {
                match PrKey::parse(token) {
                    Some(k) => found.push(Dep::Pr(k)),
                    None => found.push(Dep::Invalid(token.to_string())),
                }
            }
        }
    }
    found
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_dotnet_manifest(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".csproj")
        || lower.ends_with(".fsproj")
        || lower.ends_with(".props")
        || lower.ends_with(".targets")
}

fn is_package_json(path: &str) -> bool {
    file_name(path).eq_ignore_ascii_case("package.json")
}

pub fn keeps_patch(path: &str) -> bool {
    is_dotnet_manifest(path) || is_package_json(path)
}

fn dotnet_versions(patch: &str, prefix: char) -> BTreeMap<String, String> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static ATTR: OnceLock<Regex> = OnceLock::new();
    let tag = regex(&TAG, r"<Package(?:Reference|Version)\b([^>]*)>");
    let attr = regex(&ATTR, r#"(\w+)\s*=\s*"([^"]*)""#);
    let mut found = BTreeMap::new();
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix(prefix) else {
            continue;
        };
        let Some(tag_caps) = tag.captures(rest) else {
            continue;
        };
        let mut name = None;
        let mut version = None;
        for a in attr.captures_iter(&tag_caps[1]) {
            match a[1].to_lowercase().as_str() {
                "include" | "update" => name = Some(a[2].to_string()),
                "version" | "versionoverride" => version = Some(a[2].to_string()),
                _ => {}
            }
        }
        if let (Some(n), Some(v)) = (name, version) {
            found.insert(n.to_lowercase(), v);
        }
    }
    found
}

fn npm_versions(patch: &str, prefix: char) -> BTreeMap<String, String> {
    static PAIR: OnceLock<Regex> = OnceLock::new();
    static SEMVER: OnceLock<Regex> = OnceLock::new();
    let pair = regex(&PAIR, r#"^\s*"([^"]+)"\s*:\s*"([^"]+)""#);
    let semver = regex(
        &SEMVER,
        r"^(?:workspace:|npm:)?[\^~>=<\s]*v?\d+(?:\.[\dx*]+)*(?:[-+][\w.+-]+)?$",
    );
    let mut found = BTreeMap::new();
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix(prefix) else {
            continue;
        };
        let Some(caps) = pair.captures(rest) else {
            continue;
        };
        let name = caps[1].to_lowercase();
        if name == "name" || name == "version" || !semver.is_match(&caps[2]) {
            continue;
        }
        found.insert(name, caps[2].to_string());
    }
    found
}

pub fn bumped_packages(files: &[ChangedFile]) -> BTreeSet<String> {
    let mut bumped = BTreeSet::new();
    for file in files {
        let Some(patch) = file.patch.as_deref() else {
            continue;
        };
        let (added, removed) = if is_dotnet_manifest(&file.path) {
            (dotnet_versions(patch, '+'), dotnet_versions(patch, '-'))
        } else if is_package_json(&file.path) {
            (npm_versions(patch, '+'), npm_versions(patch, '-'))
        } else {
            continue;
        };
        for (name, version) in added {
            if removed.get(&name) != Some(&version) {
                bumped.insert(name);
            }
        }
    }
    bumped
}

pub fn published_package(path: &str, text: &str) -> Option<String> {
    if is_package_json(path) {
        let json: serde_json::Value = serde_json::from_str(text).ok()?;
        if json.get("private").and_then(|v| v.as_bool()) == Some(true) {
            return None;
        }
        let name = json.get("name")?.as_str()?.trim();
        return (!name.is_empty()).then(|| name.to_lowercase());
    }
    if path.to_lowercase().ends_with("proj") {
        static ID: OnceLock<Regex> = OnceLock::new();
        let id = regex(&ID, r"<PackageId>\s*([^<]+?)\s*</PackageId>");
        let caps = id.captures(text)?;
        let value = caps[1].to_string();
        if value.contains("$(") {
            let stem = file_name(path).rsplit_once('.').map(|(s, _)| s)?;
            return Some(stem.to_lowercase());
        }
        return Some(value.to_lowercase());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, patch: &str) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            patch: Some(patch.to_string()),
        }
    }

    #[test]
    fn depends_on_reads_short_and_full_forms() {
        let body =
            "Fix.\n\nDepends-On: acme/lib#12\ndepends-on: https://github.com/acme/api/pull/7, #3\n";
        let deps = depends_on(body, "acme/front");
        assert_eq!(
            deps,
            vec![
                Dep::Pr(PrKey::new("acme", "lib", 12)),
                Dep::Pr(PrKey::new("acme", "api", 7)),
                Dep::Pr(PrKey::new("acme", "front", 3)),
            ]
        );
    }

    #[test]
    fn depends_on_ignores_html_comments_and_flags_garbage() {
        let body = "<!-- Depends-On: org/repo#n -->\nDepends-On: soon\n";
        let deps = depends_on(body, "acme/front");
        assert_eq!(deps, vec![Dep::Invalid("soon".to_string())]);
    }

    #[test]
    fn csproj_version_bump_is_detected() {
        let patch = "@@ -10,3 +10,3 @@\n-    <PackageReference Include=\"Acme.Auth.Client\" Version=\"2.1.0\" />\n+    <PackageReference Include=\"Acme.Auth.Client\" Version=\"2.2.0\" />\n     <PackageReference Include=\"Newtonsoft.Json\" Version=\"13.0.3\" />";
        let bumped = bumped_packages(&[file("src/App/App.csproj", patch)]);
        assert_eq!(
            bumped.into_iter().collect::<Vec<_>>(),
            vec!["acme.auth.client".to_string()]
        );
    }

    #[test]
    fn unchanged_version_is_not_a_bump() {
        let patch = "-    <PackageReference Include=\"A\" Version=\"1.0.0\"/>\n+    <PackageReference Include=\"A\" Version=\"1.0.0\" PrivateAssets=\"all\"/>";
        assert!(bumped_packages(&[file("a.csproj", patch)]).is_empty());
    }

    #[test]
    fn central_package_versions_are_detected() {
        let patch = "-    <PackageVersion Include=\"Acme.Core\" Version=\"1.0.0\" />\n+    <PackageVersion Include=\"Acme.Core\" Version=\"1.1.0\" />";
        let bumped = bumped_packages(&[file("Directory.Packages.props", patch)]);
        assert!(bumped.contains("acme.core"));
    }

    #[test]
    fn package_json_ignores_scripts_and_own_version() {
        let patch = "-  \"version\": \"1.0.0\",\n+  \"version\": \"1.0.1\",\n-    \"build\": \"tsc\",\n+    \"build\": \"tsc -b\",\n-    \"@acme/ui-kit\": \"^1.4.0\",\n+    \"@acme/ui-kit\": \"^1.5.0\",";
        let bumped = bumped_packages(&[file("web/package.json", patch)]);
        assert_eq!(
            bumped.into_iter().collect::<Vec<_>>(),
            vec!["@acme/ui-kit".to_string()]
        );
    }

    #[test]
    fn lock_files_are_ignored() {
        let patch = "+    \"@acme/ui-kit\": \"1.5.0\",";
        assert!(bumped_packages(&[file("package-lock.json", patch)]).is_empty());
    }

    #[test]
    fn publisher_from_csproj_and_package_json() {
        let csproj = "<Project><PropertyGroup><PackageId>Acme.Auth.Client</PackageId></PropertyGroup></Project>";
        assert_eq!(
            published_package("src/Auth/Auth.csproj", csproj),
            Some("acme.auth.client".to_string())
        );
        let pkg = "{\"name\":\"@acme/ui-kit\",\"version\":\"1.0.0\"}";
        assert_eq!(
            published_package("package.json", pkg),
            Some("@acme/ui-kit".to_string())
        );
        let private = "{\"name\":\"app\",\"private\":true}";
        assert_eq!(published_package("package.json", private), None);
    }

    #[test]
    fn publisher_with_variable_id_falls_back_to_file_stem() {
        let csproj = "<PackageId>$(AssemblyName)</PackageId>";
        assert_eq!(
            published_package("src/Acme.Util/Acme.Util.csproj", csproj),
            Some("acme.util".to_string())
        );
    }
}
