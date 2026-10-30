use anyhow::{Result, bail};
use globset::GlobSet;
use serde::Deserialize;

use super::build_set;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderConfig {
    pub repos: Vec<String>,
}

pub fn compile(lists: &[OrderConfig]) -> Result<Vec<Vec<GlobSet>>> {
    let mut out = Vec::new();
    for (index, list) in lists.iter().enumerate() {
        if list.repos.is_empty() {
            bail!("[[order]] number {} has no repos", index + 1);
        }
        let what = format!("[[order]] number {}", index + 1);
        let entries = list
            .repos
            .iter()
            .map(|pattern| build_set(std::slice::from_ref(pattern), &what))
            .collect::<Result<Vec<_>>>()?;
        out.push(entries);
    }
    Ok(out)
}

pub fn ranks(lists: &[Vec<GlobSet>], slug: &str) -> Vec<Option<usize>> {
    lists
        .iter()
        .map(|entries| entries.iter().position(|set| set.is_match(slug)))
        .collect()
}
