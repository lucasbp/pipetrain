use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::detect;

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn stem(path: &str) -> Option<String> {
    let (stem, _) = file_name(path).rsplit_once('.')?;
    Some(stem.to_lowercase())
}

fn is_project(path: &str) -> bool {
    path.to_lowercase().ends_with("proj")
}

fn is_package_json(path: &str) -> bool {
    file_name(path).eq_ignore_ascii_case("package.json")
}

fn is_props(path: &str) -> bool {
    path.to_lowercase().ends_with(".props")
}

fn values(text: &str, tag: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let open = format!("<{}>", tag.to_lowercase());
    let close = format!("</{}>", tag.to_lowercase());
    let mut found = Vec::new();
    let mut rest = lower.as_str();
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(after[..end].trim().to_string());
        rest = &after[end + close.len()..];
    }
    found
}

fn flag_is(text: &str, tag: &str, wanted: &str) -> bool {
    values(text, tag).iter().any(|v| v == wanted)
}

fn is_test_project(path: &str, text: &str) -> bool {
    let name = stem(path).unwrap_or_default();
    name.ends_with(".tests")
        || name.ends_with(".test")
        || text.to_lowercase().contains("microsoft.net.test.sdk")
}

fn implicit_id(path: &str, text: &str) -> Option<String> {
    let packs =
        flag_is(text, "GeneratePackageOnBuild", "true") || flag_is(text, "IsPackable", "true");
    if !packs {
        return None;
    }
    let assembly = values(text, "AssemblyName")
        .into_iter()
        .find(|name| !name.is_empty() && !name.contains("$("));
    assembly.or_else(|| stem(path))
}

pub fn published(path: &str, text: &str) -> Option<String> {
    if !is_project(path) {
        return detect::published_package(path, text);
    }
    if flag_is(text, "IsPackable", "false") || is_test_project(path, text) {
        return None;
    }
    detect::published_package(path, text).or_else(|| implicit_id(path, text))
}

fn npm_references(text: &str) -> BTreeSet<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return BTreeSet::new();
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|section| json.get(section)?.as_object())
        .flat_map(|map| map.keys())
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect()
}

fn dotnet_references(text: &str) -> BTreeSet<String> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static ATTR: OnceLock<Regex> = OnceLock::new();
    let tag = regex(&TAG, r"(?i)<Package(?:Reference|Version)\b([^>]*)>");
    let attr = regex(&ATTR, r#"(?i)\b(?:include|update)\s*=\s*"([^"]*)""#);
    tag.captures_iter(text)
        .filter_map(|caps| Some(attr.captures(&caps[1])?[1].trim().to_lowercase()))
        .filter(|name| !name.is_empty() && !name.contains("$("))
        .collect()
}

pub fn references(path: &str, text: &str) -> BTreeSet<String> {
    if is_package_json(path) {
        return npm_references(text);
    }
    if is_project(path) || is_props(path) {
        return dotnet_references(text);
    }
    BTreeSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(found: BTreeSet<String>) -> Vec<String> {
        found.into_iter().collect()
    }

    #[test]
    fn explicit_package_id_wins() {
        let text = "<PackageId>Acme.Util</PackageId>";
        assert_eq!(
            published("src/Acme.Util.csproj", text).as_deref(),
            Some("acme.util")
        );
    }

    #[test]
    fn generate_package_on_build_publishes_the_file_stem() {
        let text = "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>";
        assert_eq!(
            published("src/Acme.Util.csproj", text).as_deref(),
            Some("acme.util")
        );
    }

    #[test]
    fn assembly_name_beats_the_file_stem() {
        let text = "<IsPackable>true</IsPackable><AssemblyName>Acme.Core</AssemblyName>";
        assert_eq!(
            published("src/Util.csproj", text).as_deref(),
            Some("acme.core")
        );
    }

    #[test]
    fn variable_assembly_name_falls_back_to_the_stem() {
        let text = "<IsPackable>true</IsPackable><AssemblyName>$(Root).Core</AssemblyName>";
        assert_eq!(
            published("src/Acme.Core.csproj", text).as_deref(),
            Some("acme.core")
        );
    }

    #[test]
    fn a_plain_project_publishes_nothing() {
        let text = "<TargetFramework>net9.0</TargetFramework>";
        assert_eq!(published("src/Acme.App.csproj", text), None);
    }

    #[test]
    fn not_packable_is_excluded_even_with_a_package_id() {
        let text = "<PackageId>Acme.Util</PackageId><IsPackable>false</IsPackable>";
        assert_eq!(published("src/Acme.Util.csproj", text), None);
    }

    #[test]
    fn test_projects_are_excluded() {
        let text = "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>";
        assert_eq!(published("tests/Acme.Util.Tests.csproj", text), None);
        assert_eq!(published("tests/Acme.Util.Test.csproj", text), None);
        let sdk =
            "<IsPackable>true</IsPackable><PackageReference Include=\"Microsoft.NET.Test.Sdk\" />";
        assert_eq!(published("tests/Acme.Checks.csproj", sdk), None);
    }

    #[test]
    fn variable_package_id_still_falls_back_to_the_stem() {
        let text = "<PackageId>$(Prefix).Util</PackageId>";
        assert_eq!(
            published("src/Acme.Util.csproj", text).as_deref(),
            Some("acme.util")
        );
    }

    #[test]
    fn package_json_publisher_is_unchanged() {
        assert_eq!(
            published("package.json", r#"{"name":"@Acme/Sdk"}"#).as_deref(),
            Some("@acme/sdk")
        );
        assert_eq!(
            published("package.json", r#"{"name":"acme","private":true}"#),
            None
        );
    }

    #[test]
    fn csproj_references_cover_include_and_update() {
        let text = r#"<ItemGroup>
  <PackageReference Include="Acme.Util" Version="1.0.0" />
  <PackageReference Update="Acme.Core" Version="2.0.0" />
  <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  <PackageReference Include="$(Dynamic)" />
</ItemGroup>"#;
        assert_eq!(
            names(references("src/App.csproj", text)),
            vec!["acme.core", "acme.util", "newtonsoft.json"]
        );
    }

    #[test]
    fn props_references_are_read() {
        let text = r#"<PackageVersion Include="Acme.Util" Version="1.0.0" />"#;
        assert_eq!(
            names(references("Directory.Packages.props", text)),
            vec!["acme.util"]
        );
    }

    #[test]
    fn references_ignore_attribute_order_and_case() {
        let text = r#"<packagereference Version="1.0" INCLUDE="Acme.Util"/>"#;
        assert_eq!(names(references("a.csproj", text)), vec!["acme.util"]);
    }

    #[test]
    fn npm_references_cover_the_three_sections() {
        let text = r#"{"name":"acme-web","dependencies":{"@Acme/Sdk":"^1.0.0"},"devDependencies":{"vite":"^5"},"peerDependencies":{"react":"^18"},"scripts":{"build":"x"}}"#;
        assert_eq!(
            names(references("web/package.json", text)),
            vec!["@acme/sdk", "react", "vite"]
        );
    }

    #[test]
    fn broken_json_and_other_files_reference_nothing() {
        assert!(references("package.json", "{").is_empty());
        assert!(references("README.md", "Include=\"Acme.Util\"").is_empty());
    }
}
