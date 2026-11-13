use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::Value;

use super::*;

#[derive(Clone)]
struct Child {
    org: String,
    repo: String,
    base: String,
    head: String,
    merged: String,
}

#[derive(Clone, Default)]
pub(super) struct Evidence(Arc<Mutex<Vec<Child>>>);

impl Evidence {
    pub(super) fn record(&self, item: &Item, head: &str, merged: &str) {
        self.0.lock().unwrap().push(Child {
            org: item.org.clone(),
            repo: item.repo.clone(),
            base: item.base.clone(),
            head: head.to_string(),
            merged: merged.to_string(),
        });
    }

    fn last_child(&self, item: &Item) -> Option<Child> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|child| {
                child.org.eq_ignore_ascii_case(&item.org)
                    && child.repo.eq_ignore_ascii_case(&item.repo)
                    && child.base == item.head
            })
            .cloned()
    }

    pub(super) fn has_child(&self, item: &Item) -> bool {
        self.last_child(item).is_some()
    }

    pub(super) fn accepts(&self, item: &Item, client: &mut Client, head: &str) -> Result<bool> {
        let Some(child) = self.last_child(item) else {
            return Ok(false);
        };
        if head.is_empty() || item.sha.is_empty() {
            return Ok(false);
        }
        let slug = format!("{}/{}", item.org, item.repo);
        if !client.contains_commit(&slug, head, &item.sha)? {
            return Ok(false);
        }
        if child.merged == head {
            return Ok(true);
        }
        let commit = client.rest(&format!("/repos/{slug}/git/commits/{head}"))?;
        let Some(parents) = commit.get("parents").and_then(Value::as_array) else {
            return Ok(false);
        };
        if parents.len() != 2 {
            return Ok(false);
        }
        let parents: Vec<_> = parents
            .iter()
            .filter_map(|parent| parent.get("sha").and_then(Value::as_str))
            .collect();
        if !parents.contains(&item.sha.as_str())
            || !parents.contains(&child.head.as_str())
            || child.head == item.sha
        {
            return Ok(false);
        }
        let comparison = client.rest(&format!(
            "/repos/{slug}/compare/{}...{head}?per_page=1",
            child.merged
        ))?;
        Ok(
            comparison.get("status").and_then(Value::as_str) == Some("ahead")
                && comparison.get("ahead_by").and_then(Value::as_u64) == Some(1),
        )
    }
}
