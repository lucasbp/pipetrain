use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Waiting,
    Blocked,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Waiting => "waiting",
            Category::Blocked => "blocked",
        }
    }
}

pub const DEPENDS_PREFIX: &str = "depends on ";

pub fn depends_on(id: &str, parent: Category) -> String {
    format!("{DEPENDS_PREFIX}{id}, which is {}", parent.as_str())
}

pub fn deploy_failed_cause(detail: &str) -> String {
    let at = detail
        .strip_prefix("workflow ")
        .and_then(|rest| rest.split_once(" ended as "))
        .map(|(name, tail)| {
            let conclusion = tail.split(':').next().unwrap_or(tail).trim();
            format!(" at workflow {name} ({conclusion})")
        })
        .unwrap_or_default();
    format!("deploy failed{at}, needs a new commit")
}

fn derived(lower: &str) -> bool {
    lower.starts_with("branch protection blocks merge")
        || lower.starts_with("checks are not passing")
}

fn waiting_reason(lower: &str) -> bool {
    matches!(
        lower,
        "review required" | "not approved" | "changes requested" | "checks pending"
    ) || lower == "draft"
        || lower.starts_with("draft (")
        || lower.starts_with("waiting for package ")
        || lower.starts_with(crate::planner::behind::PREFIX)
        || lower.starts_with(DEPENDS_PREFIX)
        || lower.starts_with("deploy failed")
        || lower.contains(" depends on ")
}

pub fn classify(reasons: &[String]) -> Category {
    let lowered: Vec<String> = reasons
        .iter()
        .map(|reason| reason.to_ascii_lowercase())
        .collect();
    let Some(first) = lowered.first() else {
        return Category::Blocked;
    };
    if first.starts_with(DEPENDS_PREFIX) || first.starts_with("deploy failed") {
        return Category::Waiting;
    }
    let primary: Vec<&String> = lowered.iter().filter(|reason| !derived(reason)).collect();
    if primary.is_empty() {
        return Category::Blocked;
    }
    if primary.iter().all(|reason| waiting_reason(reason)) {
        Category::Waiting
    } else {
        Category::Blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(reasons: &[&str]) -> Category {
        classify(&reasons.iter().map(|r| (*r).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn human_action_causes_wait() {
        for cause in [
            "review required",
            "not approved",
            "changes requested",
            "draft",
            "checks pending",
            "waiting for package acme.core 1.2.0",
            "needs a bump of acme.core to 1.4.0",
            "acme/web#2 depends on acme/api#1",
        ] {
            assert_eq!(of(&[cause]), Category::Waiting, "{cause}");
        }
    }

    #[test]
    fn broken_causes_block() {
        for cause in [
            "checks failing",
            "checks failing: build, lint",
            "no checks reported",
            "merge conflict",
            "merge conflict (DIRTY)",
            "branch behind its base, update it",
            "branch protection blocks merge (BLOCKED)",
            "checks are not passing (UNSTABLE)",
            "already merged",
            "closed",
            "already in acme/api#9",
            "something unknown",
        ] {
            assert_eq!(of(&[cause]), Category::Blocked, "{cause}");
        }
    }

    #[test]
    fn derived_causes_follow_the_primary_one() {
        assert_eq!(
            of(&[
                "review required",
                "branch protection blocks merge (BLOCKED)"
            ]),
            Category::Waiting
        );
        assert_eq!(
            of(&["checks pending", "checks are not passing (UNSTABLE)"]),
            Category::Waiting
        );
        assert_eq!(of(&["draft", "checks failing: build"]), Category::Blocked);
    }

    #[test]
    fn deploy_failed_cause_names_the_workflow() {
        let cause =
            deploy_failed_cause("workflow deploy ended as failure: https://example.test/run/1");
        assert_eq!(
            cause,
            "deploy failed at workflow deploy (failure), needs a new commit"
        );
        assert_eq!(of(&[&cause]), Category::Waiting);
        assert_eq!(
            deploy_failed_cause("anything else"),
            "deploy failed, needs a new commit"
        );
    }
}
