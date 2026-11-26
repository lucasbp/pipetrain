use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::command::{RepoSnapshot, bump};
use crate::detect;
use crate::feed::{Coordinate, Ecosystem};
use crate::model::Pr;
use crate::plan::Plan;

#[derive(Default)]
pub(in crate::command) struct Restrictions {
    blocked: BTreeMap<(String, Coordinate), String>,
    publishers: BTreeMap<(String, String), BTreeSet<(Ecosystem, String)>>,
}

impl Restrictions {
    pub(in crate::command) fn record_publishers(&mut self, repo: &str, snapshot: &RepoSnapshot) {
        for (_, path, text) in &snapshot.manifests {
            let Some(normalized) = crate::manifest::published(path, text) else {
                continue;
            };
            let (ecosystem, name) = if detect::is_go_mod(path) {
                (
                    Ecosystem::Go,
                    crate::gomod::published_name(text).unwrap_or_default(),
                )
            } else {
                let ecosystem = if detect::is_cargo_toml(path) {
                    Ecosystem::Cargo
                } else if detect::is_pyproject(path) {
                    Ecosystem::Pypi
                } else if path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case("package.json"))
                {
                    Ecosystem::Npm
                } else {
                    Ecosystem::Nuget
                };
                (ecosystem, normalized.clone())
            };
            self.publishers
                .entry((repo.to_lowercase(), normalized))
                .or_default()
                .insert((ecosystem, name));
        }
    }

    fn publisher_mismatch(&self, repo: &str, key: &str, c: &Coordinate) -> bool {
        self.publishers
            .get(&(repo.to_lowercase(), key.to_owned()))
            .is_some_and(|found| {
                let name = if c.ecosystem == Ecosystem::Go {
                    c.package.as_str()
                } else {
                    key
                };
                !found.contains(&(c.ecosystem, name.to_owned()))
            })
    }

    pub(in crate::command) fn inspect(&mut self, pr: &Pr, sha: &str, snapshot: &RepoSnapshot) {
        for file in &pr.files {
            if !(detect::is_cargo_toml(&file.path)
                || detect::is_pyproject(&file.path)
                || detect::is_go_mod(&file.path))
            {
                continue;
            }
            for bump in detect::bumps(std::slice::from_ref(file)).into_values() {
                let Some(coord) = bump.feed else { continue };
                let reason = if pr.head_sha != sha || snapshot.incomplete {
                    Some("the consumer head has no complete registry-source snapshot".into())
                } else {
                    source_issue(snapshot, &file.path, &coord)
                };
                if let Some(reason) = reason {
                    self.blocked.insert((pr.key.to_string(), coord), reason);
                }
            }
        }
    }

    pub(in crate::command) fn apply(self, plan: &mut Plan) {
        for item in &mut plan.items {
            for dep in &mut item.after {
                let Some(c) = &dep.feed_coordinate else {
                    continue;
                };
                let reason = self
                    .blocked
                    .get(&(item.id.clone(), c.clone()))
                    .cloned()
                    .or_else(|| {
                        let (repo, _) = dep.id.rsplit_once('#')?;
                        let key = dep.package.as_deref()?;
                        self.publisher_mismatch(repo, key, c).then(|| {
                            format!(
                                "{repo} publishes {key} under a different ecosystem or module path"
                            )
                        })
                    });
                if let Some(reason) = reason {
                    dep.feed_coordinate = None;
                    dep.feed_restriction = Some(reason);
                }
            }
        }
    }
}

fn source_issue(snapshot: &RepoSnapshot, path: &str, c: &Coordinate) -> Option<String> {
    let companions: &[&str] = match c.ecosystem {
        Ecosystem::Cargo => &[".cargo/config", ".cargo/config.toml"],
        Ecosystem::Pypi => &["uv.toml"],
        Ecosystem::Go => &["go.work"],
        _ => return None,
    };
    let parent = Path::new(path).parent()?;
    for dir in parent.ancestors() {
        for name in companions {
            let config = dir.join(name);
            if snapshot.tree.iter().any(|p| Path::new(p) == config) {
                return Some(format!(
                    "registry source is governed by {}",
                    config.display()
                ));
            }
        }
    }
    let read = |path: &str| {
        snapshot
            .manifests
            .iter()
            .find(|(_, p, _)| p == path)
            .map(|(_, _, body)| body)
    };
    let Some(body) = read(path) else {
        return Some(format!("cannot verify registry source in {path}"));
    };
    if !bump::public_dependency(c.ecosystem, body, &c.package) {
        return Some(format!(
            "{path} has no unambiguous public source for {}",
            c.package
        ));
    }
    bump::check_workspace_sources(c.ecosystem, &c.package, path, body, &snapshot.tree, |p| {
        read(p)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("could not read {p}"))
    })
    .err()
    .map(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_mapping_needs_no_manifest_but_a_conflicting_publisher_cannot_prove_a_wait() {
        let mut guard = Restrictions::default();
        let cargo = Coordinate {
            ecosystem: Ecosystem::Cargo,
            package: "kit".into(),
            version: "1.0.0".into(),
        };
        assert!(!guard.publisher_mismatch("acme/lib", "kit", &cargo));
        let snapshot = RepoSnapshot {
            head: "a".repeat(40),
            incomplete: false,
            tree: vec![],
            manifests: vec![
                (
                    "acme/lib".into(),
                    "pyproject.toml".into(),
                    "[project]\nname='kit'".into(),
                ),
                (
                    "acme/lib".into(),
                    "go.mod".into(),
                    "module example.org/Kit\n".into(),
                ),
            ],
        };
        guard.record_publishers("acme/lib", &snapshot);
        assert!(guard.publisher_mismatch("acme/lib", "kit", &cargo));
        let go = Coordinate {
            ecosystem: Ecosystem::Go,
            package: "example.org/kit".into(),
            version: "v1.0.0".into(),
        };
        assert!(guard.publisher_mismatch("acme/lib", "example.org/kit", &go));
        let go = Coordinate {
            package: "example.org/Kit".into(),
            ..go
        };
        assert!(!guard.publisher_mismatch("acme/lib", "example.org/kit", &go));
    }

    #[test]
    fn source_evidence_follows_the_consumers_ancestors_and_package() {
        for (ecosystem, name, filename, config, body, alternate) in [
            (
                Ecosystem::Cargo,
                "kit",
                "Cargo.toml",
                ".cargo/config.toml",
                "[dependencies]\nkit = '1.0.0'\nother = { version = '1.0.0',registry = 'private' }\n",
                "[dependencies]\nkit = {version='1.0.0',registry='private'}",
            ),
            (
                Ecosystem::Pypi,
                "kit",
                "pyproject.toml",
                "uv.toml",
                "[project]\ndependencies=['kit>=1.0']\n[tool.uv.sources]\nother = { path='../local' }",
                "[project]\ndependencies=['kit==1.0']\n[tool.uv.sources]\nkit={path='../local'}",
            ),
            (
                Ecosystem::Go,
                "example.org/kit",
                "go.mod",
                "go.work",
                "module example.org/app\nrequire example.org/kit v1.0.0\nreplace example.org/other => ../local",
                "module example.org/app\nrequire example.org/kit v1.0.0\nreplace example.org/kit => ../local",
            ),
        ] {
            let coordinate = Coordinate {
                ecosystem,
                package: name.into(),
                version: "1.0.0".into(),
            };
            let path = format!("workspace/service/{filename}");
            let sibling = format!("other/{filename}");
            let mut snapshot = RepoSnapshot {
                head: "b".repeat(40),
                incomplete: false,
                tree: vec![path.clone(), sibling.clone(), format!("other/{config}")],
                manifests: vec![
                    ("a/b".into(), path.clone(), body.into()),
                    ("a/b".into(), sibling, alternate.into()),
                ],
            };
            assert!(
                source_issue(&snapshot, &path, &coordinate).is_none(),
                "{ecosystem:?}"
            );
            snapshot.tree.push(format!("workspace/{config}"));
            assert!(
                source_issue(&snapshot, &path, &coordinate)
                    .unwrap()
                    .contains("governed")
            );
            snapshot.tree.pop();
            snapshot.manifests[0].2 = alternate.into();
            assert!(
                source_issue(&snapshot, &path, &coordinate)
                    .unwrap()
                    .contains("public source")
            );
        }
    }
}
