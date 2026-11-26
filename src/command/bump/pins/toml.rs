use std::sync::LazyLock;

use ::toml::Spanned;
use ::toml::de::{DeTable, DeValue};
use anyhow::Result;
use regex::Regex;

use super::{Pin, version_part};
use crate::feed::Ecosystem;

type Node<'i> = Spanned<DeValue<'i>>;

fn table<'a, 'i>(root: &'a DeTable<'i>, path: &[&str]) -> Option<&'a DeTable<'i>> {
    path.iter()
        .try_fold(root, |t, key| t.get(*key)?.get_ref().as_table())
}

fn tables<'a, 'i>(root: &'a DeTable<'i>, key: &str) -> impl Iterator<Item = &'a DeTable<'i>> {
    table(root, &[key])
        .into_iter()
        .flat_map(|t| t.values())
        .filter_map(|v| v.get_ref().as_table())
}

fn from_string(ecosystem: Ecosystem, text: &str, node: &Node<'_>, spec: &str) -> Pin {
    let decoded = node.get_ref().as_str().expect("a dependency string");
    let raw = &text[node.span()];
    let Some(quote @ ('\'' | '"')) = raw.chars().next() else {
        return Pin::manual("manual: nonliteral dependency");
    };
    let Some(version) = version_part(ecosystem, spec) else {
        return Pin::manual(spec);
    };
    let relative = version.as_ptr() as usize - decoded.as_ptr() as usize;
    let prefix = &decoded[..relative + version.len()];
    if !raw
        .strip_prefix(quote)
        .is_some_and(|body| body.starts_with(prefix) && !body.starts_with(quote))
    {
        return Pin::manual("manual: escaped or multiline dependency");
    }
    let start = node.span().start + 1 + relative;
    Pin {
        redirected: false,
        current: spec.to_owned(),
        span: Some(start..start + version.len()),
    }
}

fn cargo_name(name: &str) -> String {
    name.to_ascii_lowercase().replace('_', "-")
}

fn name_of(key: &str, node: &Node<'_>) -> String {
    cargo_name(
        node.get_ref()
            .get("package")
            .and_then(|n| n.get_ref().as_str())
            .unwrap_or(key),
    )
}

pub(super) fn patched(root: &DeTable<'_>, package: &str) -> bool {
    let name = cargo_name(package);
    tables(root, "patch").any(|t| {
        t.iter()
            .any(|(key, node)| name_of(key.get_ref(), node) == name)
    }) || table(root, &["replace"]).is_some_and(|t| {
        t.keys().any(|key| {
            let id = key.get_ref().rsplit('#').next().unwrap_or_default();
            cargo_name(id.split(':').next().unwrap_or_default()) == name
        })
    })
}

fn cargo_pins(text: &str, root: &DeTable<'_>, package: &str) -> Vec<Pin> {
    let name = cargo_name(package);
    let patched = patched(root, package);
    let scopes = std::iter::once(root)
        .chain(table(root, &["workspace"]))
        .chain(tables(root, "target"));
    let mut pins = Vec::new();
    for scope in scopes {
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(deps) = table(scope, &[section]) else {
                continue;
            };
            for (key, node) in deps {
                if name_of(key.get_ref(), node) != name {
                    continue;
                }
                let entry = node.get_ref();
                if patched
                    || ["path", "git", "registry", "registry-index", "workspace"]
                        .iter()
                        .any(|key| entry.get(*key).is_some())
                {
                    pins.push(Pin::alternate(
                        "manual: inherited or alternate Cargo source",
                    ));
                } else {
                    let value = entry.get("version").unwrap_or(node);
                    if let Some(spec) = value.get_ref().as_str() {
                        pins.push(from_string(Ecosystem::Cargo, text, value, spec.trim()));
                    }
                }
            }
        }
    }
    pins
}

pub(super) fn uv_source(root: &DeTable<'_>, package: &str) -> bool {
    let name = crate::pyproject::normalise(package);
    let uv = table(root, &["tool", "uv"]);
    table(root, &["tool", "uv", "sources"]).is_some_and(|t| {
        t.keys()
            .any(|key| crate::pyproject::normalise(key.get_ref()) == name)
    }) || uv.is_some_and(|t| {
        ["index-url", "extra-index-url", "find-links"]
            .iter()
            .any(|k| t.contains_key(*k))
            || t.get("index")
                .and_then(|v| v.get_ref().as_array())
                .is_some_and(|indexes| {
                    indexes.iter().any(|v| {
                        v.get_ref()
                            .get("explicit")
                            .and_then(|v| v.get_ref().as_bool())
                            != Some(true)
                    })
                })
    }) || ["override-dependencies", "constraint-dependencies"]
        .iter()
        .any(|key| {
            uv.and_then(|t| t.get(*key))
                .and_then(|v| v.get_ref().as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.get_ref().as_str())
                .any(|requirement| {
                    let requirement = requirement.trim_start();
                    let end = requirement
                        .find(|c: char| {
                            !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                        })
                        .unwrap_or(requirement.len());
                    crate::pyproject::normalise(&requirement[..end]) == name
                })
        })
}

fn python_pins(text: &str, root: &DeTable<'_>, package: &str) -> Vec<Pin> {
    static REQUIREMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[[^\]]+\]\s*)?([^;]*)").unwrap()
    });
    let name = crate::pyproject::normalise(package);
    let mut pins = Vec::new();
    let mut alternate = uv_source(root, package)
        || ["poetry", "pdm"]
            .into_iter()
            .any(|tool| table(root, &["tool", tool]).is_some_and(|t| t.contains_key("source")));
    let poetry = table(root, &["tool", "poetry"]);
    let groups = poetry.into_iter().flat_map(|p| tables(p, "group"));
    for scope in poetry.into_iter().chain(groups) {
        for section in ["dependencies", "dev-dependencies"] {
            let Some(deps) = table(scope, &[section]) else {
                continue;
            };
            for (key, node) in deps {
                if key.get_ref().eq_ignore_ascii_case("python")
                    || crate::pyproject::normalise(key.get_ref()) != name
                {
                    continue;
                }
                let entry = node.get_ref();
                if entry.as_array().is_some()
                    || ["path", "git", "url", "source"]
                        .iter()
                        .any(|k| entry.get(*k).is_some())
                {
                    alternate = true;
                    pins.push(Pin::alternate(
                        "manual: alternate or conditional Poetry source",
                    ));
                } else {
                    let value = entry.get("version").unwrap_or(node);
                    if let Some(spec) = value.get_ref().as_str() {
                        pins.push(from_string(Ecosystem::Pypi, text, value, spec.trim()));
                    }
                }
            }
        }
    }
    let project = table(root, &["project"]);
    let main = project.and_then(|p| p.get("dependencies"));
    let optional = project.and_then(|p| table(p, &["optional-dependencies"]));
    let groups = table(root, &["dependency-groups"]);
    let build = table(root, &["build-system"]).and_then(|t| t.get("requires"));
    let lists = main
        .into_iter()
        .chain(build)
        .chain(optional.into_iter().chain(groups).flat_map(|t| t.values()));
    for node in lists.flat_map(|n| n.get_ref().as_array()).flatten() {
        let Some(captures) = node
            .get_ref()
            .as_str()
            .and_then(|s| REQUIREMENT.captures(s))
        else {
            continue;
        };
        if crate::pyproject::normalise(&captures[1]) != name {
            continue;
        }
        let raw = captures
            .get(2)
            .expect("requirement specifier")
            .as_str()
            .trim();
        let spec = raw
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(raw)
            .trim();
        if spec.starts_with('@') {
            pins.push(Pin::alternate("manual: direct Python dependency"));
        } else if spec.starts_with("==") {
            pins.push(from_string(Ecosystem::Pypi, text, node, spec));
        } else {
            pins.push(Pin::manual(if spec.starts_with(['<', '>', '!', '~']) {
                spec
            } else {
                "manual: no exact Python requirement"
            }));
        }
    }
    if alternate {
        for pin in &mut pins {
            *pin = Pin::alternate("manual: alternate or conditional Python source");
        }
    }
    pins
}

pub(super) fn pins(ecosystem: Ecosystem, text: &str, package: &str) -> Result<Vec<Pin>> {
    let root = DeTable::parse(text)?;
    Ok(match ecosystem {
        Ecosystem::Cargo => cargo_pins(text, root.get_ref(), package),
        Ecosystem::Pypi => python_pins(text, root.get_ref(), package),
        _ => unreachable!(),
    })
}
