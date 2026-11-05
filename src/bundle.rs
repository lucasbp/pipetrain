use std::collections::BTreeMap;

use serde::Serialize;

use crate::planner::Item;

#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    pub repo: String,
    pub base: String,
    pub branch: String,
    pub existing_train_pr: Option<String>,
    pub retarget: Vec<String>,
    pub deploys_now: usize,
    pub deploys_after: usize,
    pub saved: usize,
}

pub fn is_stacked(base: &str, train_prefix: &str) -> bool {
    base.to_lowercase()
        .starts_with(&train_prefix.to_lowercase())
}

pub fn is_train_branch(name: &str, train: &str) -> bool {
    let name = name.to_lowercase();
    let train = train.to_lowercase();
    name == train || name.starts_with(&format!("{train}-"))
}

pub fn base_suffix(base: &str) -> String {
    let mut out = String::new();
    for c in base.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

pub fn suggest(items: &[Item], deploy_branch: &str, train_prefix: &str) -> Vec<Bundle> {
    let mut groups: BTreeMap<(String, String), Vec<&Item>> = BTreeMap::new();
    for item in items.iter().filter(|i| !is_stacked(&i.base, train_prefix)) {
        let slug = format!("{}/{}", item.org, item.repo).to_lowercase();
        groups
            .entry((slug, item.base.clone()))
            .or_default()
            .push(item);
    }
    let groups: Vec<((String, String), Vec<&Item>)> = groups
        .into_iter()
        .filter(|(_, group)| group.len() >= 2)
        .collect();
    let mut bases_per_repo: BTreeMap<&str, usize> = BTreeMap::new();
    for ((slug, _), _) in &groups {
        *bases_per_repo.entry(slug.as_str()).or_default() += 1;
    }
    let named: Vec<String> = groups
        .iter()
        .map(|((slug, base), _)| {
            if bases_per_repo.get(slug.as_str()).copied().unwrap_or(0) > 1 {
                format!("{deploy_branch}-{}", base_suffix(base))
            } else {
                deploy_branch.to_string()
            }
        })
        .collect();
    groups
        .into_iter()
        .zip(named)
        .map(|((_, group), branch)| {
            let existing = group.iter().find(|i| i.head == branch);
            let retarget: Vec<String> = group
                .iter()
                .filter(|i| i.head != branch)
                .map(|i| i.id.clone())
                .collect();
            Bundle {
                repo: format!("{}/{}", group[0].org, group[0].repo),
                base: group[0].base.clone(),
                branch,
                existing_train_pr: existing.map(|i| i.id.clone()),
                retarget,
                deploys_now: group.len(),
                deploys_after: 1,
                saved: group.len() - 1,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_becomes_a_flat_lowercase_suffix() {
        assert_eq!(base_suffix("master"), "master");
        assert_eq!(base_suffix("feature/New_Thing"), "feature-new_thing");
        assert_eq!(base_suffix("release//2.0"), "release-2.0");
        assert_eq!(base_suffix("/x/"), "x");
    }

    #[test]
    fn a_train_branch_is_the_train_or_the_train_with_a_base_suffix() {
        let train = "deploy/05-11-2026";
        assert!(is_train_branch("deploy/05-11-2026", train));
        assert!(is_train_branch("Deploy/05-11-2026-feature-x", train));
        assert!(!is_train_branch("deploy/05-11-2026x", train));
        assert!(!is_train_branch("deploy/06-11-2026", train));
    }
}
