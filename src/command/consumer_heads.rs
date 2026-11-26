use super::*;
use crate::planner::behind::{Need, QueuedBump};
use anyhow::ensure;

pub(super) fn has_external_release(context: &release::Context, repo: &str) -> bool {
    context.nuget_releases.iter().any(|package| {
        context
            .publishers
            .get(package)
            .is_some_and(|owners| !owners.contains(repo))
    })
}

struct Snapshot {
    context: release::Context,
    manifests: Vec<release::Manifest>,
}

fn nuget_paths(tree: &[String]) -> (Vec<String>, bool) {
    let relevant = tree
        .iter()
        .filter(|path| bump::in_ecosystem(path, crate::feed::Ecosystem::Nuget))
        .cloned()
        .collect::<Vec<_>>();
    publishers::candidate_paths(&relevant)
}

pub(super) fn incomplete(snapshot: &RepoSnapshot) -> bool {
    if snapshot.tree_truncated || !snapshot.incomplete {
        return snapshot.tree_truncated;
    }
    let (paths, cut) = nuget_paths(&snapshot.tree);
    cut || paths
        .iter()
        .any(|path| !snapshot.manifests.iter().any(|(_, read, _)| read == path))
}

impl Snapshot {
    fn read(
        client: &mut Client,
        context: &release::Context,
        repo: &str,
        sha: &str,
    ) -> Result<Self> {
        let (tree, truncated) = client.tree(repo, sha)?;
        let (paths, cut) = nuget_paths(&tree);
        ensure!(!truncated && !cut, "incomplete NuGet manifest snapshot");
        let blobs = client.blobs(repo, sha, &paths)?;
        ensure!(
            blobs.len() == paths.len(),
            "incomplete NuGet manifest snapshot"
        );
        let manifests: Vec<_> = blobs
            .into_iter()
            .map(|(path, body)| (repo.to_string(), path, body))
            .collect();
        let mut head = context.consumer_head(repo, &manifests);
        if !behind::consumers(&head).is_empty() {
            let files = workflows::read_from_tree(client, repo, sha, &tree)?;
            let files = crate::deploy_resources::with_reusables(client, repo, sha, files);
            head.limits
                .insert(repo.to_string(), workflows::max_behind(&files));
        }
        Ok(Self {
            context: head,
            manifests,
        })
    }
}

#[derive(Default)]
pub(super) struct Heads(BTreeMap<(String, String), Result<Snapshot>>);

impl Heads {
    pub(super) fn read(
        client: &mut Client,
        context: &release::Context,
        primary: &BTreeMap<(String, String), bool>,
        ready: &[(Pr, Scope)],
    ) -> Self {
        let mut heads = Self::default();
        for (pr, _) in ready {
            let repo = pr.key.slug().to_lowercase();
            if !has_external_release(context, &repo) {
                continue;
            }
            let key = (repo.clone(), pr.head_sha.clone());
            if primary.get(&key) == Some(&false) {
                continue;
            }
            heads.0.entry(key.clone()).or_insert_with(|| {
                ensure!(!primary.contains_key(&key), "incomplete manifest snapshot");
                Snapshot::read(client, context, &repo, &pr.head_sha)
            });
        }
        heads
    }

    pub(super) fn candidates(&self) -> BTreeMap<String, Vec<String>> {
        self.0
            .values()
            .filter_map(|head| head.as_ref().ok())
            .flat_map(|head| behind::candidates(&head.context))
            .collect()
    }

    pub(super) fn context(&self, org: &str, repo: &str, sha: &str) -> Option<&release::Context> {
        self.0
            .get(&(format!("{org}/{repo}").to_lowercase(), sha.to_string()))?
            .as_ref()
            .ok()
            .map(|head| &head.context)
    }

    pub(super) fn hold(
        &self,
        primary: &release::Context,
        feeds: &behind::Feeds,
        manifests: &[release::Manifest],
        ready: &mut Vec<(Pr, Scope)>,
        skipped: &mut Vec<Skipped>,
    ) -> (Vec<Need>, Vec<QueuedBump>) {
        let mut needs = Vec::new();
        let mut contracts = Vec::new();
        for (pr, scope) in std::mem::take(ready) {
            let key = (pr.key.slug().to_lowercase(), pr.head_sha.clone());
            let (context, manifests) = match self.0.get(&key) {
                Some(Ok(head)) => (&head.context, head.manifests.as_slice()),
                Some(Err(error)) => {
                    let reason = format!(
                        "cannot verify consumer pins at head {}: {error}",
                        pr.head_sha
                    );
                    skipped.push(Skipped {
                        pr,
                        scope,
                        reasons: vec![reason],
                    });
                    continue;
                }
                None => (primary, manifests),
            };
            let selected = (pr, scope);
            let row = std::slice::from_ref(&selected);
            let mut held = behind::find(context, row, feeds);
            if self.0.contains_key(&key) {
                held.extend(
                    behind::find(primary, row, feeds)
                        .into_iter()
                        .filter(|need| !primary.nuget_releases.contains(&need.package)),
                );
            }
            contracts.extend(behind::queued_bumps(&held, context, &selected.0, manifests));
            needs.extend(held);
            ready.push(selected);
        }
        behind::hold_needs(&needs, ready, skipped);
        (needs, contracts)
    }
}
