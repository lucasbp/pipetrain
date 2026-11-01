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

fn npm_versions(text: &str, package: &str) -> Vec<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|section| json.get(section)?.as_object())
        .flat_map(|map| map.iter())
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case(package))
        .filter_map(|(_, spec)| spec.as_str())
        .map(|spec| spec.trim().to_string())
        .collect()
}

fn dotnet_versions(text: &str, package: &str) -> Vec<String> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    static VERSION: OnceLock<Regex> = OnceLock::new();
    static BODY: OnceLock<Regex> = OnceLock::new();
    static CHILD: OnceLock<Regex> = OnceLock::new();
    let tag = regex(&TAG, r"(?i)<Package(?:Reference|Version)\b([^>]*)>");
    let name = regex(&NAME, r#"(?i)\b(?:include|update)\s*=\s*"([^"]*)""#);
    let version = regex(
        &VERSION,
        r#"(?i)\b(?:version|versionoverride)\s*=\s*"([^"]*)""#,
    );
    let body = regex(&BODY, r"(?is)^(.*?)</Package(?:Reference|Version)\s*>");
    let child = regex(
        &CHILD,
        r"(?is)<Version(?:Override)?>\s*([^<]*?)\s*</Version(?:Override)?>",
    );
    tag.captures_iter(text)
        .filter_map(|caps| {
            let attrs = &caps[1];
            if !name.captures(attrs)?[1]
                .trim()
                .eq_ignore_ascii_case(package)
            {
                return None;
            }
            if let Some(found) = version.captures(attrs) {
                return Some(found[1].trim().to_string());
            }
            if attrs.trim_end().ends_with('/') {
                return None;
            }
            let rest = &text[caps.get(0)?.end()..];
            let inner = body.captures(rest)?;
            Some(child.captures(&inner[1])?[1].trim().to_string())
        })
        .filter(|found| !found.is_empty())
        .collect()
}

pub fn package_versions(path: &str, text: &str, package: &str) -> Vec<String> {
    if is_package_json(path) {
        return npm_versions(text, package);
    }
    if is_project(path) || is_props(path) {
        return dotnet_versions(text, package);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csproj_reads_attribute_and_child_element_versions() {
        let text = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Acme.Util" Version="1.2.3" />
    <PackageReference Include="acme.util" VersionOverride="1.4.0"/>
    <PackageReference Include="Acme.Util">
      <Version>1.5.0</Version>
    </PackageReference>
    <PackageReference Include="Acme.Other" Version="9.9.9" />
  </ItemGroup>
</Project>"#;
        assert_eq!(
            package_versions("src/App/App.csproj", text, "Acme.Util"),
            vec!["1.2.3", "1.4.0", "1.5.0"]
        );
    }

    #[test]
    fn central_package_management_reads_the_props_file() {
        let text = r#"<Project><ItemGroup>
<PackageVersion Include="Acme.Util" Version="2.0.0" />
<PackageVersion Include="Acme.Other" Version="1.0.0" />
</ItemGroup></Project>"#;
        assert_eq!(
            package_versions("Directory.Packages.props", text, "Acme.Util"),
            vec!["2.0.0"]
        );
    }

    #[test]
    fn a_reference_without_a_version_has_nothing_to_report() {
        let text =
            r#"<Project><ItemGroup><PackageReference Include="Acme.Util" /></ItemGroup></Project>"#;
        assert!(package_versions("App.csproj", text, "Acme.Util").is_empty());
    }

    #[test]
    fn package_json_reads_every_dependency_section_with_the_raw_spec() {
        let text = r#"{
  "dependencies": {"@acme/ui": "^1.2.0", "left-pad": "1.0.0"},
  "devDependencies": {"@acme/ui": "1.3.0"},
  "peerDependencies": {"@acme/ui": "~1.0.0"}
}"#;
        assert_eq!(
            package_versions("web/package.json", text, "@acme/ui"),
            vec!["^1.2.0", "1.3.0", "~1.0.0"]
        );
    }

    #[test]
    fn other_files_and_invalid_json_report_nothing() {
        assert!(package_versions("README.md", "Acme.Util 1.0.0", "Acme.Util").is_empty());
        assert!(package_versions("package.json", "{ nope", "left-pad").is_empty());
    }
}
