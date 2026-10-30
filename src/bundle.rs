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

pub fn suggest(items: &[Item], deploy_branch: &str, train_prefix: &str) -> Vec<Bundle> {
    let mut groups: BTreeMap<(String, String), Vec<&Item>> = BTreeMap::new();
    for item in items.iter().filter(|i| !is_stacked(&i.base, train_prefix)) {
        let slug = format!("{}/{}", item.org, item.repo).to_lowercase();
        groups
            .entry((slug, item.base.clone()))
            .or_default()
            .push(item);
    }
    groups
        .into_values()
        .filter(|group| group.len() >= 2)
        .map(|group| {
            let existing = group.iter().find(|i| i.head == deploy_branch);
            let retarget: Vec<String> = group
                .iter()
                .filter(|i| i.head != deploy_branch)
                .map(|i| i.id.clone())
                .collect();
            Bundle {
                repo: format!("{}/{}", group[0].org, group[0].repo),
                base: group[0].base.clone(),
                branch: deploy_branch.to_string(),
                existing_train_pr: existing.map(|i| i.id.clone()),
                retarget,
                deploys_now: group.len(),
                deploys_after: 1,
                saved: group.len() - 1,
            }
        })
        .collect()
}
