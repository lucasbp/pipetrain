use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::detect::side_lines;

const OTHER_DIRECTIVES: [&str; 8] = [
    "go",
    "module",
    "toolchain",
    "replace",
    "exclude",
    "retract",
    "godebug",
    "tool",
];

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn without_comment(line: &str) -> &str {
    line.split("//").next().unwrap_or(line).trim()
}

fn module_name(raw: &str) -> String {
    raw.trim().trim_matches('"').to_lowercase()
}

pub fn published(text: &str) -> Option<String> {
    static MODULE: OnceLock<Regex> = OnceLock::new();
    let module = regex(&MODULE, r"^module\s+(\S+)");
    text.lines()
        .map(without_comment)
        .find_map(|line| module.captures(line).map(|caps| module_name(&caps[1])))
        .filter(|name| !name.is_empty())
}

#[derive(Clone, Copy, PartialEq)]
enum Block {
    Unknown,
    Require,
    Other,
    Outside,
}

fn requirement(line: &str) -> Option<(String, String)> {
    static PAIR: OnceLock<Regex> = OnceLock::new();
    let pair = regex(&PAIR, r"^(\S+)\s+(v\S*)$");
    if line.contains("=>") {
        return None;
    }
    let caps = pair.captures(line)?;
    Some((module_name(&caps[1]), caps[2].to_string()))
}

fn walk<'a>(
    lines: impl Iterator<Item = (bool, &'a str)>,
    mut visit: impl FnMut(bool, String, String),
) {
    static OPEN: OnceLock<Regex> = OnceLock::new();
    let open = regex(&OPEN, r"^([a-z]+)\s*\($");
    let mut block = Block::Unknown;
    for (changed, body) in lines {
        let line = without_comment(body);
        if line.is_empty() {
            continue;
        }
        if line == ")" {
            block = Block::Outside;
            continue;
        }
        if let Some(caps) = open.captures(line) {
            block = if &caps[1] == "require" {
                Block::Require
            } else {
                Block::Other
            };
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or_default();
        let entry = if first == "require" {
            requirement(line.trim_start_matches("require").trim())
        } else if OTHER_DIRECTIVES.contains(&first)
            || matches!(block, Block::Other | Block::Outside)
        {
            None
        } else {
            requirement(line)
        };
        if let Some((name, version)) = entry {
            visit(changed, name, version);
        }
    }
}

pub fn references(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk(text.lines().map(|line| (true, line)), |_, name, _| {
        found.insert(name);
    });
    found
}

fn readable(version: &str) -> String {
    static PSEUDO: OnceLock<Regex> = OnceLock::new();
    let pseudo = regex(&PSEUDO, r"-\d{14}-[0-9a-f]{12}$");
    if pseudo.is_match(version) {
        return String::new();
    }
    version
        .strip_suffix("+incompatible")
        .unwrap_or(version)
        .to_string()
}

pub fn bumped(patch: &str, prefix: char) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    walk(side_lines(patch, prefix), |changed, name, version| {
        if changed {
            found.insert(name, readable(&version));
        }
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_line_is_the_published_name() {
        let text = "module github.com/Acme/Util/v2 // comment\n\ngo 1.22\n";
        assert_eq!(published(text).as_deref(), Some("github.com/acme/util/v2"));
        assert_eq!(published("go 1.22\n"), None);
    }

    #[test]
    fn references_read_blocks_and_single_lines_but_not_replace() {
        let text = "module x\n\nrequire github.com/acme/one v1.0.0\n\nrequire (\n\tgithub.com/acme/two v1.2.3 // indirect\n)\n\nreplace github.com/acme/three => ../three\n";
        let found: Vec<String> = references(text).into_iter().collect();
        assert_eq!(found, vec!["github.com/acme/one", "github.com/acme/two"]);
    }
}
