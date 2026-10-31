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
