use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;
use toml::{Table, Value};

use crate::detect::side_lines;

const DEPENDENCY_SECTIONS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

const PACKAGE_KEYS: [&str; 18] = [
    "name",
    "version",
    "edition",
    "rust-version",
    "description",
    "license",
    "license-file",
    "readme",
    "repository",
    "homepage",
    "documentation",
    "resolver",
    "build",
    "links",
    "publish",
    "default-run",
    "channel",
    "profile",
];

const LOCAL_KEYS: [&str; 3] = ["path", "git", "workspace"];

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn normalise(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

pub fn published(text: &str) -> Option<String> {
    let root: Table = toml::from_str(text).ok()?;
    let package = root.get("package")?.as_table()?;
    match package.get("publish") {
        Some(Value::Boolean(false)) => return None,
        Some(Value::Array(registries)) if registries.is_empty() => return None,
        _ => {}
    }
    let name = package.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| normalise(name))
}

fn dependency_tables(root: &Table) -> Vec<&Table> {
    let mut scopes = vec![root];
    scopes.extend(root.get("workspace").and_then(Value::as_table));
    if let Some(targets) = root.get("target").and_then(Value::as_table) {
        scopes.extend(targets.values().filter_map(Value::as_table));
    }
    scopes
        .into_iter()
        .flat_map(|scope| {
            DEPENDENCY_SECTIONS
                .iter()
                .filter_map(move |section| scope.get(*section)?.as_table())
        })
        .collect()
}

pub fn references(text: &str) -> BTreeSet<String> {
    let Ok(root) = toml::from_str::<Table>(text) else {
        return BTreeSet::new();
    };
    dependency_tables(&root)
        .into_iter()
        .flat_map(|table| table.iter())
        .map(|(key, value)| {
            let real = value
                .as_table()
                .and_then(|spec| spec.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(key);
            normalise(real)
        })
        .filter(|name| !name.is_empty())
        .collect()
}

enum Section {
    Unknown,
    Other,
    Dependencies,
    Table,
}

struct Pending {
    name: String,
    version: Option<String>,
    local: bool,
}

fn section_of(header: &str) -> (Section, Option<String>) {
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static PLAIN: OnceLock<Regex> = OnceLock::new();
    let named = regex(
        &NAMED,
        r"(?:^|\.)(?:dependencies|dev-dependencies|build-dependencies)\.([^.]+)$",
    );
    let plain = regex(
        &PLAIN,
        r"(?:^|\.)(?:dependencies|dev-dependencies|build-dependencies)$",
    );
    if let Some(caps) = named.captures(header) {
        let name = caps[1].trim_matches('"').to_string();
        return (Section::Table, Some(normalise(&name)));
    }
    if plain.is_match(header) {
        return (Section::Dependencies, None);
    }
    (Section::Other, None)
}

fn flush(pending: Option<Pending>, found: &mut BTreeMap<String, String>) {
    if let Some(Pending {
        name,
        version: Some(version),
        local: false,
    }) = pending
    {
        found.insert(name, version);
    }
}

fn string_value(value: &str) -> Option<String> {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    let quoted = regex(&QUOTED, r#"^"([^"]*)""#);
    Some(quoted.captures(value.trim())?[1].trim().to_string())
}

fn inline_entry(key: &str, value: &str) -> Option<(String, String)> {
    let wrapped: Table = toml::from_str(&format!("v = {value}")).ok()?;
    let spec = wrapped.get("v")?.as_table()?;
    if LOCAL_KEYS.iter().any(|local| spec.contains_key(*local)) {
        return None;
    }
    let version = spec.get("version")?.as_str()?.trim();
    let real = spec.get("package").and_then(Value::as_str).unwrap_or(key);
    (!version.is_empty()).then(|| (normalise(real), version.to_string()))
}

pub fn bumped(patch: &str, prefix: char) -> BTreeMap<String, String> {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    static PAIR: OnceLock<Regex> = OnceLock::new();
    let header = regex(&HEADER, r"^\[([^\[\]]+)\]\s*(?:#.*)?$");
    let pair = regex(&PAIR, r#"^("[^"]+"|[A-Za-z0-9_.-]+)\s*=\s*(.+)$"#);
    let mut found = BTreeMap::new();
    let mut section = Section::Unknown;
    let mut pending: Option<Pending> = None;
    for (changed, body) in side_lines(patch, prefix) {
        let line = body.trim();
        if let Some(caps) = header.captures(line) {
            flush(pending.take(), &mut found);
            let (next, name) = section_of(caps[1].trim());
            section = next;
            pending = name.map(|name| Pending {
                name,
                version: None,
                local: false,
            });
            continue;
        }
        let Some(caps) = pair.captures(line) else {
            continue;
        };
        let key = caps[1].trim_matches('"');
        let value = caps[2].trim();
        match section {
            Section::Other => {}
            Section::Table => {
                let Some(open) = pending.as_mut() else {
                    continue;
                };
                if LOCAL_KEYS.contains(&key) {
                    open.local = true;
                } else if key == "package" {
                    if let Some(real) = string_value(value) {
                        open.name = normalise(&real);
                    }
                } else if key == "version" && changed {
                    open.version = string_value(value).filter(|v| !v.is_empty());
                }
            }
            Section::Dependencies | Section::Unknown => {
                if !changed {
                    continue;
                }
                if value.starts_with('{') {
                    if let Some((name, version)) = inline_entry(key, value) {
                        found.insert(name, version);
                    }
                } else if let Some(version) = string_value(value) {
                    let is_setting = matches!(section, Section::Unknown)
                        && PACKAGE_KEYS.contains(&key.to_lowercase().as_str());
                    if !version.is_empty() && !is_setting {
                        found.insert(normalise(key), version);
                    }
                }
            }
        }
    }
    flush(pending.take(), &mut found);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_is_published_unless_publish_is_off() {
        let text = "[package]\nname = \"Acme_Util\"\nversion = \"1.0.0\"\n";
        assert_eq!(published(text).as_deref(), Some("acme-util"));
        let off = "[package]\nname = \"acme\"\npublish = false\n";
        assert_eq!(published(off), None);
        let none = "[package]\nname = \"acme\"\npublish = []\n";
        assert_eq!(published(none), None);
        assert_eq!(published("[workspace]\nmembers = [\"a\"]\n"), None);
    }

    #[test]
    fn references_cover_every_dependency_table_and_renames() {
        let text = r#"[dependencies]
acme-util = "1"
alias = { package = "acme-core", version = "2" }

[dev-dependencies]
acme_test = { path = "../test" }

[workspace.dependencies]
acme-shared = "3"

[target.'cfg(unix)'.dependencies]
acme-unix = "1"
"#;
        let found: Vec<String> = references(text).into_iter().collect();
        assert_eq!(
            found,
            vec![
                "acme-core",
                "acme-shared",
                "acme-test",
                "acme-unix",
                "acme-util"
            ]
        );
    }

    #[test]
    fn a_table_dependency_reads_its_version_when_the_header_is_in_the_patch() {
        let patch =
            " [dependencies.acme-util]\n-version = \"1.0.0\"\n+version = \"1.1.0\"\n features = []";
        assert_eq!(bumped(patch, '+')["acme-util"], "1.1.0");
        assert_eq!(bumped(patch, '-')["acme-util"], "1.0.0");
    }
}
