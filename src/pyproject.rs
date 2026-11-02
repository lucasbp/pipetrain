use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;
use toml::{Table, Value};

use crate::detect::side_lines;

const LOCAL_KEYS: [&str; 3] = ["path", "git", "url"];

const SETTING_KEYS: [&str; 13] = [
    "name",
    "version",
    "description",
    "readme",
    "license",
    "python",
    "requires-python",
    "authors",
    "homepage",
    "repository",
    "documentation",
    "build-backend",
    "package-mode",
];

const PRIVATE_CLASSIFIER: &str = "private :: do not upload";

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn normalise(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn requirement(text: &str) -> Option<(String, String)> {
    static PARTS: OnceLock<Regex> = OnceLock::new();
    let parts = regex(
        &PARTS,
        r"^\s*([A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?)\s*(?:\[[^\]]*\])?\s*(.*)$",
    );
    let caps = parts.captures(text)?;
    let rest = caps[2].split(';').next().unwrap_or_default().trim();
    let spec = rest
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(rest)
        .trim();
    let valid = spec.is_empty() || spec.starts_with(['<', '>', '=', '!', '~', '@']);
    valid.then(|| (normalise(&caps[1]), spec.to_string()))
}

fn raw_version(spec: &str) -> Option<String> {
    if spec.is_empty() || spec.starts_with('@') {
        return None;
    }
    if spec.contains(',') {
        return Some(spec.to_string());
    }
    for operator in ["===", "==", "~=", ">=", ">"] {
        if let Some(version) = spec.strip_prefix(operator) {
            return Some(version.trim().to_string());
        }
    }
    Some(spec.to_string())
}

fn is_private(root: &Table) -> bool {
    let uv_off = root
        .get("tool")
        .and_then(|tool| tool.get("uv"))
        .and_then(|uv| uv.get("package"))
        .and_then(Value::as_bool)
        == Some(false);
    let private_class = root
        .get("project")
        .and_then(|project| project.get("classifiers"))
        .and_then(Value::as_array)
        .is_some_and(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .any(|c| c.trim().to_lowercase() == PRIVATE_CLASSIFIER)
        });
    uv_off || private_class
}

pub fn published(text: &str) -> Option<String> {
    let root: Table = toml::from_str(text).ok()?;
    if is_private(&root) {
        return None;
    }
    let poetry = root
        .get("tool")
        .and_then(|tool| tool.get("poetry"))
        .and_then(Value::as_table);
    let from_project = root
        .get("project")
        .and_then(|project| project.get("name"))
        .and_then(Value::as_str);
    let from_poetry = poetry
        .filter(|table| table.get("package-mode").and_then(Value::as_bool) != Some(false))
        .and_then(|table| table.get("name"))
        .and_then(Value::as_str);
    let name = from_project.or(from_poetry)?.trim();
    (!name.is_empty()).then(|| normalise(name))
}

fn strings(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn grouped<'a>(root: &'a Table, path: &[&str]) -> Vec<&'a Value> {
    let mut node = Some(root);
    let (last, head) = path.split_last().expect("non-empty path");
    for key in head {
        node = node
            .and_then(|table| table.get(*key))
            .and_then(Value::as_table);
    }
    node.and_then(|table| table.get(*last))
        .and_then(Value::as_table)
        .map(|table| table.values().collect())
        .unwrap_or_default()
}

pub fn references(text: &str) -> BTreeSet<String> {
    let Ok(root) = toml::from_str::<Table>(text) else {
        return BTreeSet::new();
    };
    let mut found = BTreeSet::new();
    let project = root.get("project");
    let mut requirements: Vec<&str> = strings(project.and_then(|p| p.get("dependencies")));
    for list in grouped(&root, &["project", "optional-dependencies"]) {
        requirements.extend(strings(Some(list)));
    }
    for list in grouped(&root, &["dependency-groups"]) {
        requirements.extend(strings(Some(list)));
    }
    requirements.extend(strings(
        root.get("build-system").and_then(|b| b.get("requires")),
    ));
    found.extend(
        requirements
            .into_iter()
            .filter_map(requirement)
            .map(|(name, _)| name),
    );
    let poetry = root.get("tool").and_then(|tool| tool.get("poetry"));
    let mut tables: Vec<&Table> = Vec::new();
    for key in ["dependencies", "dev-dependencies"] {
        tables.extend(poetry.and_then(|p| p.get(key)).and_then(Value::as_table));
    }
    if let Some(groups) = poetry
        .and_then(|p| p.get("group"))
        .and_then(Value::as_table)
    {
        tables.extend(
            groups
                .values()
                .filter_map(|group| group.get("dependencies")?.as_table()),
        );
    }
    found.extend(
        tables
            .into_iter()
            .flat_map(|table| table.keys())
            .filter(|key| !key.eq_ignore_ascii_case("python"))
            .map(|key| normalise(key)),
    );
    found
}

enum Section {
    Unknown,
    Other,
    Requirements,
    Poetry,
    Table,
}

struct Pending {
    name: String,
    version: Option<String>,
    local: bool,
}

fn section_of(header: &str) -> (Section, Option<String>) {
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static POETRY: OnceLock<Regex> = OnceLock::new();
    static LISTS: OnceLock<Regex> = OnceLock::new();
    let named = regex(
        &NAMED,
        r"^tool\.poetry\.(?:dependencies|dev-dependencies|group\.[^.]+\.dependencies)\.([^.]+)$",
    );
    let poetry = regex(
        &POETRY,
        r"^tool\.poetry\.(?:dependencies|dev-dependencies|group\.[^.]+\.dependencies)$",
    );
    let lists = regex(
        &LISTS,
        r"^(?:project|project\.optional-dependencies|dependency-groups|build-system|tool\.uv)$",
    );
    if let Some(caps) = named.captures(header) {
        return (Section::Table, Some(normalise(caps[1].trim_matches('"'))));
    }
    if poetry.is_match(header) {
        return (Section::Poetry, None);
    }
    if lists.is_match(header) {
        return (Section::Requirements, None);
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

fn poetry_entry(key: &str, value: &str) -> Option<(String, String)> {
    if key.eq_ignore_ascii_case("python") {
        return None;
    }
    if value.starts_with('{') {
        let wrapped: Table = toml::from_str(&format!("v = {value}")).ok()?;
        let spec = wrapped.get("v")?.as_table()?;
        if LOCAL_KEYS.iter().any(|local| spec.contains_key(*local)) {
            return None;
        }
        let version = spec.get("version")?.as_str()?.trim();
        return (!version.is_empty()).then(|| (normalise(key), version.to_string()));
    }
    let version = string_value(value)?;
    (!version.is_empty()).then(|| (normalise(key), version))
}

fn quoted_requirements(line: &str) -> Vec<(String, String)> {
    static ITEM: OnceLock<Regex> = OnceLock::new();
    let item = regex(&ITEM, r#""((?:[^"\\]|\\.)*)""#);
    item.captures_iter(line)
        .filter_map(|caps| requirement(&caps[1]))
        .filter_map(|(name, spec)| Some((name, raw_version(&spec)?)))
        .collect()
}

pub fn bumped(patch: &str, prefix: char) -> BTreeMap<String, String> {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    static PAIR: OnceLock<Regex> = OnceLock::new();
    static ARRAY: OnceLock<Regex> = OnceLock::new();
    let header = regex(&HEADER, r"^\[([^\[\]]+)\]\s*(?:#.*)?$");
    let pair = regex(&PAIR, r#"^("[^"]+"|[A-Za-z0-9_.-]+)\s*=\s*(.+)$"#);
    let array = regex(&ARRAY, r"^[A-Za-z0-9_.-]+\s*=\s*\[");
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
        let list_line = line.starts_with('"') || array.is_match(line);
        let pair_caps = pair.captures(line);
        match section {
            Section::Other => {}
            Section::Requirements => {
                if changed && list_line {
                    found.extend(quoted_requirements(line));
                }
            }
            Section::Poetry | Section::Unknown => {
                if !changed {
                    continue;
                }
                if list_line {
                    if matches!(section, Section::Unknown) {
                        found.extend(quoted_requirements(line));
                    }
                    continue;
                }
                let Some(caps) = pair_caps else {
                    continue;
                };
                let key = caps[1].trim_matches('"');
                let is_setting = matches!(section, Section::Unknown)
                    && SETTING_KEYS.contains(&key.to_lowercase().as_str());
                if is_setting {
                    continue;
                }
                if let Some((name, version)) = poetry_entry(key, caps[2].trim()) {
                    found.insert(name, version);
                }
            }
            Section::Table => {
                let (Some(open), Some(caps)) = (pending.as_mut(), pair_caps) else {
                    continue;
                };
                let key = &caps[1];
                if LOCAL_KEYS.contains(&key) {
                    open.local = true;
                } else if key == "version" && changed {
                    open.version = string_value(&caps[2]).filter(|v| !v.is_empty());
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
    fn project_name_wins_and_private_projects_publish_nothing() {
        let text = "[project]\nname = \"Acme.Util_Core\"\n";
        assert_eq!(published(text).as_deref(), Some("acme-util-core"));
        let poetry = "[tool.poetry]\nname = \"acme-lib\"\n";
        assert_eq!(published(poetry).as_deref(), Some("acme-lib"));
        let off = "[tool.poetry]\nname = \"acme\"\npackage-mode = false\n";
        assert_eq!(published(off), None);
        let private = "[project]\nname = \"acme\"\nclassifiers = [\"Private :: Do Not Upload\"]\n";
        assert_eq!(published(private), None);
    }

    #[test]
    fn requirements_keep_a_single_bound_and_drop_the_rest() {
        let found = quoted_requirements(
            r#"dependencies = ["acme-util>=1.2.3", "acme-core==2.0", "acme-low<2", "acme-box>=1,<2", "acme-git @ git+https://x/y"]"#,
        );
        assert_eq!(
            found,
            vec![
                ("acme-util".to_string(), "1.2.3".to_string()),
                ("acme-core".to_string(), "2.0".to_string()),
                ("acme-low".to_string(), "<2".to_string()),
                ("acme-box".to_string(), ">=1,<2".to_string()),
            ]
        );
    }
}
