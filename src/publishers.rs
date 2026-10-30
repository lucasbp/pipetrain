use std::collections::{BTreeMap, BTreeSet};

use crate::detect;

pub type Publishers = BTreeMap<String, BTreeSet<String>>;

pub const MAX_MANIFESTS_PER_REPO: usize = 40;

pub fn candidate_paths(tree: &[String]) -> (Vec<String>, bool) {
    let mut paths: Vec<String> = tree
        .iter()
        .filter(|p| {
            let lower = p.to_lowercase();
            let manifest = lower.ends_with(".csproj")
                || lower.ends_with(".fsproj")
                || lower == "package.json"
                || lower.ends_with("/package.json");
            let noise = p
                .split('/')
                .any(|s| matches!(s, "node_modules" | "bin" | "obj" | ".git"));
            manifest && !noise
        })
        .cloned()
        .collect();
    paths.sort_by_key(|p| (p.matches('/').count(), p.clone()));
    let truncated = paths.len() > MAX_MANIFESTS_PER_REPO;
    paths.truncate(MAX_MANIFESTS_PER_REPO);
    (paths, truncated)
}

pub fn collect(manifests: &[(String, String, String)]) -> Publishers {
    let mut found = Publishers::new();
    for (slug, path, text) in manifests {
        if let Some(name) = detect::published_package(path, text) {
            found.entry(name).or_default().insert(slug.to_lowercase());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_skip_noise_and_prefer_shallow_paths() {
        let tree: Vec<String> = [
            "README.md",
            "package.json",
            "web/node_modules/x/package.json",
            "src/Lib/Lib.csproj",
            "src/Lib/bin/Debug/Lib.csproj",
            "docs/readme.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            candidate_paths(&tree),
            (
                vec!["package.json".to_string(), "src/Lib/Lib.csproj".to_string()],
                false
            )
        );
    }

    #[test]
    fn candidates_flag_when_manifests_were_cut() {
        let tree: Vec<String> = (0..=MAX_MANIFESTS_PER_REPO)
            .map(|i| format!("pkg{i}/package.json"))
            .collect();
        let (paths, truncated) = candidate_paths(&tree);
        assert_eq!(paths.len(), MAX_MANIFESTS_PER_REPO);
        assert!(truncated);
    }

    #[test]
    fn collect_groups_repos_by_package() {
        let manifests = vec![
            (
                "Acme/Lib".to_string(),
                "Lib.csproj".to_string(),
                "<PackageId>Acme.Lib</PackageId>".to_string(),
            ),
            (
                "acme/fork".to_string(),
                "Lib.csproj".to_string(),
                "<PackageId>Acme.Lib</PackageId>".to_string(),
            ),
        ];
        let found = collect(&manifests);
        assert_eq!(found["acme.lib"].len(), 2);
    }
}
