use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{ChangedFile, PrKey};
use crate::{cargo, gomod, pyproject};

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

pub fn is_cargo_toml(path: &str) -> bool {
    file_name(path).eq_ignore_ascii_case("cargo.toml")
}

pub fn is_pyproject(path: &str) -> bool {
    file_name(path).eq_ignore_ascii_case("pyproject.toml")
}

pub fn is_go_mod(path: &str) -> bool {
    file_name(path).eq_ignore_ascii_case("go.mod")
}

pub fn keeps_patch(path: &str) -> bool {
    is_dotnet_manifest(path)
        || is_package_json(path)
        || is_cargo_toml(path)
        || is_pyproject(path)
        || is_go_mod(path)
}

pub(crate) fn side_lines(patch: &str, prefix: char) -> impl Iterator<Item = (bool, &str)> {
    patch
        .lines()
        .filter_map(move |line| match line.chars().next() {
            Some(marker) if marker == prefix => Some((true, &line[marker.len_utf8()..])),
            Some(' ') => Some((false, &line[1..])),
            _ => None,
        })
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

fn bumped_entries(files: &[ChangedFile]) -> Vec<(String, String)> {
    let mut bumped = Vec::new();
    for file in files {
        let Some(patch) = file.patch.as_deref() else {
            continue;
        };
        let (added, removed) = if is_dotnet_manifest(&file.path) {
            (dotnet_versions(patch, '+'), dotnet_versions(patch, '-'))
        } else if is_package_json(&file.path) {
            (npm_versions(patch, '+'), npm_versions(patch, '-'))
        } else if is_cargo_toml(&file.path) {
            (cargo::bumped(patch, '+'), cargo::bumped(patch, '-'))
        } else if is_pyproject(&file.path) {
            (pyproject::bumped(patch, '+'), pyproject::bumped(patch, '-'))
        } else if is_go_mod(&file.path) {
            (gomod::bumped(patch, '+'), gomod::bumped(patch, '-'))
        } else {
            continue;
        };
        for (name, version) in added {
            if removed.get(&name) != Some(&version) {
                bumped.push((name, version));
            }
        }
    }
    bumped
}

pub fn bumped_packages(files: &[ChangedFile]) -> BTreeSet<String> {
    bumped_entries(files)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

fn normalise_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    let version = ["^", "~", ">=", ">", "=", "v"]
        .iter()
        .find_map(|prefix| unbracketed.strip_prefix(prefix))
        .unwrap_or(unbracketed);
    if !version.starts_with(|c: char| c.is_ascii_digit())
        || version.contains("$(")
        || version.contains('*')
        || version.contains(',')
        || version.contains(char::is_whitespace)
    {
        return None;
    }
    let core = version.split(['-', '+']).next().unwrap_or(version);
    if core.split('.').any(|part| part.eq_ignore_ascii_case("x")) {
        return None;
    }
    Some(version.to_string())
}

pub fn bumped_versions(files: &[ChangedFile]) -> BTreeMap<String, String> {
    let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (name, raw) in bumped_entries(files) {
        let version = normalise_version(&raw);
        match seen.get(&name) {
            Some(existing) if *existing != version => {
                seen.insert(name, None);
            }
            Some(_) => {}
            None => {
                seen.insert(name, version);
            }
        }
    }
    seen.into_iter()
        .filter_map(|(name, version)| version.map(|v| (name, v)))
        .collect()
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
    fn dotnet_bumped_version_is_read() {
        let patch = "-    <PackageReference Include=\"CmUtil\" Version=\"1.2610.3022.39\" />\n+    <PackageReference Include=\"CmUtil\" Version=\"1.2610.3022.40\" />";
        let versions = bumped_versions(&[file("src/App/App.csproj", patch)]);
        assert_eq!(
            versions.get("cmutil").map(String::as_str),
            Some("1.2610.3022.40")
        );
    }

    #[test]
    fn npm_range_prefix_is_stripped() {
        let patch = "-    \"@acme/ui-kit\": \"^1.4.0\",\n+    \"@acme/ui-kit\": \"^1.5.0\",";
        let versions = bumped_versions(&[file("web/package.json", patch)]);
        assert_eq!(
            versions.get("@acme/ui-kit").map(String::as_str),
            Some("1.5.0")
        );
    }

    #[test]
    fn nuget_brackets_are_stripped() {
        let patch = "-    <PackageVersion Include=\"A\" Version=\"[1.0.0]\" />\n+    <PackageVersion Include=\"A\" Version=\"[1.2.3]\" />";
        let versions = bumped_versions(&[file("Directory.Packages.props", patch)]);
        assert_eq!(versions.get("a").map(String::as_str), Some("1.2.3"));
    }

    #[test]
    fn variable_and_wildcard_versions_are_dropped() {
        let patch = "-    <PackageReference Include=\"CmUtil\" Version=\"1.0.0\" />\n+    <PackageReference Include=\"CmUtil\" Version=\"$(CmUtilVersion)\" />";
        assert!(bumped_versions(&[file("a.csproj", patch)]).is_empty());
        let wildcard = "+    <PackageReference Include=\"B\" Version=\"1.*\" />\n+    <PackageReference Include=\"C\" Version=\"1.x\" />\n+    <PackageReference Include=\"D\" Version=\"[1.0,2.0)\" />";
        assert!(bumped_versions(&[file("a.csproj", wildcard)]).is_empty());
    }

    #[test]
    fn same_version_in_two_files_is_kept() {
        let patch = "-    <PackageReference Include=\"A\" Version=\"1.0.0\" />\n+    <PackageReference Include=\"A\" Version=\"1.1.0\" />";
        let versions = bumped_versions(&[file("a/a.csproj", patch), file("b/b.csproj", patch)]);
        assert_eq!(versions.get("a").map(String::as_str), Some("1.1.0"));
    }

    #[test]
    fn two_different_versions_are_dropped() {
        let first = "-    <PackageReference Include=\"A\" Version=\"1.0.0\" />\n+    <PackageReference Include=\"A\" Version=\"1.1.0\" />";
        let second = "-    <PackageReference Include=\"A\" Version=\"1.0.0\" />\n+    <PackageReference Include=\"A\" Version=\"1.2.0\" />";
        let versions = bumped_versions(&[file("a/a.csproj", first), file("b/b.csproj", second)]);
        assert!(versions.is_empty());
    }

    #[test]
    fn unchanged_version_has_no_entry() {
        let patch = "-    <PackageReference Include=\"A\" Version=\"1.0.0\"/>\n+    <PackageReference Include=\"A\" Version=\"1.0.0\" PrivateAssets=\"all\"/>";
        assert!(bumped_versions(&[file("a.csproj", patch)]).is_empty());
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
