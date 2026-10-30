use std::collections::{BTreeMap, BTreeSet};

use crate::manifest;

pub type Publishers = BTreeMap<String, BTreeSet<String>>;

pub type References = BTreeMap<String, BTreeSet<String>>;

pub const MAX_MANIFESTS_PER_REPO: usize = 40;

pub fn candidate_paths(tree: &[String]) -> (Vec<String>, bool) {
    let mut paths: Vec<String> = tree
        .iter()
        .filter(|p| {
            let lower = p.to_lowercase();
            let manifest = lower.ends_with(".csproj")
                || lower.ends_with(".fsproj")
                || lower.ends_with(".props")
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
        if let Some(name) = manifest::published(path, text) {
            found.entry(name).or_default().insert(slug.to_lowercase());
        }
    }
    found
}

pub fn collect_references(manifests: &[(String, String, String)]) -> References {
    let mut found = References::new();
    for (slug, path, text) in manifests {
        let names = manifest::references(path, text);
        if !names.is_empty() {
            found.entry(slug.to_lowercase()).or_default().extend(names);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_of(slug: &str, path: &str, text: &str) -> (String, String, String) {
        (slug.to_string(), path.to_string(), text.to_string())
    }

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
        let (paths, truncated) = candidate_paths(&tree);
        assert_eq!(paths, vec!["package.json", "src/Lib/Lib.csproj"]);
        assert!(!truncated);
    }

    #[test]
    fn candidates_include_props_files() {
        let tree = vec![
            "Directory.Packages.props".to_string(),
            "src/Lib/Lib.csproj".to_string(),
        ];
        let (paths, _) = candidate_paths(&tree);
        assert_eq!(
            paths,
            vec!["Directory.Packages.props", "src/Lib/Lib.csproj"]
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
            manifest_of("Acme/Lib", "Lib.csproj", "<PackageId>Acme.Lib</PackageId>"),
            manifest_of("acme/fork", "Lib.csproj", "<PackageId>Acme.Lib</PackageId>"),
        ];
        let found = collect(&manifests);
        assert_eq!(found["acme.lib"].len(), 2);
    }

    #[test]
    fn collect_publishes_a_packable_project_by_its_stem() {
        let manifests = vec![manifest_of(
            "Acme/Util",
            "src/Acme.Util.csproj",
            "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>",
        )];
        let found = collect(&manifests);
        assert_eq!(found["acme.util"].len(), 1);
        assert!(found["acme.util"].contains("acme/util"));
    }

    #[test]
    fn collect_references_groups_package_names_by_repo() {
        let manifests = vec![
            manifest_of(
                "Acme/Sdk",
                "src/Acme.Sdk.csproj",
                r#"<PackageReference Include="Acme.Util" Version="1.0.0" />"#,
            ),
            manifest_of(
                "Acme/Sdk",
                "Directory.Packages.props",
                r#"<PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />"#,
            ),
            manifest_of(
                "Acme/Web",
                "package.json",
                r#"{"dependencies":{"@acme/sdk":"1"}}"#,
            ),
            manifest_of("Acme/Docs", "package.json", r#"{"name":"docs"}"#),
        ];
        let found = collect_references(&manifests);
        assert_eq!(
            found["acme/sdk"].iter().cloned().collect::<Vec<_>>(),
            vec!["acme.util", "newtonsoft.json"]
        );
        assert!(found["acme/web"].contains("@acme/sdk"));
        assert!(!found.contains_key("acme/docs"));
    }
}
