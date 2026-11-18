use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrKey {
    pub org: String,
    pub repo: String,
    pub number: u64,
}

impl PrKey {
    pub fn new(org: &str, repo: &str, number: u64) -> Self {
        Self {
            org: org.to_string(),
            repo: repo.to_string(),
            number,
        }
    }

    pub fn slug(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    pub fn lookup_id(&self) -> String {
        self.to_string().to_lowercase()
    }

    pub fn parse(input: &str) -> Option<PrKey> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(
                r"^(?:https?://github\.com/)?([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)(?:#|/pull/)(\d+)/?$",
            )
            .expect("valid regex")
        });
        let caps = re.captures(input.trim())?;
        Some(PrKey {
            org: caps[1].to_string(),
            repo: caps[2].to_string(),
            number: caps[3].parse().ok()?,
        })
    }
}

pub fn parse_bare(input: &str) -> Option<(String, u64)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^([A-Za-z0-9_.-]+)#(\d+)$").expect("valid regex"));
    let caps = re.captures(input.trim())?;
    Some((caps[1].to_string(), caps[2].parse().ok()?))
}

impl fmt::Display for PrKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}#{}", self.org, self.repo, self.number)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    Success,
    Pending,
    Failure,
    Missing,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub patch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub key: PrKey,
    pub state: PrState,
    pub title: String,
    pub url: String,
    pub author: String,
    pub body: String,
    pub head_ref: String,
    pub base_ref: String,
    pub default_branch: String,
    pub merge_state_status: String,
    pub check_details: Vec<CheckDetail>,
    pub head_sha: String,
    pub base_sha: String,
    pub is_draft: bool,
    pub review: Review,
    pub checks: Checks,
    pub failed_checks: Vec<String>,
    pub conflicting: bool,
    pub mergeable_unknown: bool,
    pub behind: bool,
    pub files: Vec<ChangedFile>,
    pub files_complete: bool,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CheckDetail {
    pub name: String,
    pub state: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Readiness {
    pub id: String,
    pub status: String,
    pub causes: Vec<String>,
    pub next_step: String,
    pub base: String,
    pub default_branch: String,
    pub merge_state_status: String,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<String>,
    pub checks: Vec<CheckDetail>,
}

pub const NO_CHECKS_EXCEPTION: &str = "no checks reported; accepted because this repo is configured to merge without checks (allow_missing_checks or checks_optional_repos)";

impl Readiness {
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "{}: {}; base {}; default {}; cause: {}; next: {}",
            self.id,
            self.status,
            self.base,
            if self.default_branch.is_empty() {
                "unknown"
            } else {
                &self.default_branch
            },
            self.causes.join("; "),
            self.next_step
        )];
        lines.extend(
            self.warnings
                .iter()
                .map(|warning| format!("{}: warning: {warning}", self.id)),
        );
        lines.extend(
            self.exceptions
                .iter()
                .map(|exception| format!("{}: exception: {exception}", self.id)),
        );
        lines.extend(self.checks.iter().map(|check| {
            format!(
                "{}: check {}: {}{}",
                self.id,
                check.name,
                check.state,
                check
                    .url
                    .as_ref()
                    .map(|url| format!(" {url}"))
                    .unwrap_or_default()
            )
        }));
        lines
    }
}

pub fn merge_state_blocker(state: &str) -> Option<(&'static str, &'static str)> {
    match state.to_ascii_uppercase().as_str() {
        "BLOCKED" => Some((
            "branch protection blocks merge (BLOCKED)",
            "satisfy the required reviews, checks and branch protection rules, then retry",
        )),
        "DIRTY" => Some((
            "merge conflict (DIRTY)",
            "resolve conflicts with the base branch, then retry",
        )),
        "UNSTABLE" => Some((
            "checks are not passing (UNSTABLE)",
            "inspect the reported checks and fix or rerun them, then retry",
        )),
        "DRAFT" => Some(("draft (DRAFT)", "mark the PR ready for review, then retry")),
        "BEHIND" => Some((
            "branch behind its base, update it",
            "update the branch from its base and wait for checks, then retry",
        )),
        _ => None,
    }
}

const MAX_FAILED_NAMES: usize = 5;

impl Pr {
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l.eq_ignore_ascii_case(label))
    }

    fn failing_reason(&self) -> String {
        if self.failed_checks.is_empty() {
            return "checks failing".to_string();
        }
        let shown = self
            .failed_checks
            .iter()
            .take(MAX_FAILED_NAMES)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let extra = self.failed_checks.len().saturating_sub(MAX_FAILED_NAMES);
        if extra == 0 {
            format!("checks failing: {shown}")
        } else {
            format!("checks failing: {shown}, +{extra} more")
        }
    }

    pub fn readiness(&self, reasons: Vec<String>) -> Readiness {
        let next_step = if reasons.is_empty() {
            "run merge to revalidate reviews, checks, head SHA and clean mergeability".to_string()
        } else {
            let mut steps = Vec::new();
            if let Some((_, next)) = merge_state_blocker(&self.merge_state_status) {
                steps.push(next.to_string());
            }
            for reason in &reasons {
                let next = match reason.as_str() {
                    "already merged" | "closed" => "select an open PR",
                    "draft" => "mark the PR ready for review",
                    "changes requested" => "address the requested changes and obtain approval",
                    "review required" | "not approved" => "obtain the required approval",
                    "checks pending" => "wait for checks to complete",
                    "no checks reported" => {
                        "verify that the required checks are configured and reported"
                    }
                    "merge conflict" => "resolve conflicts with the base branch",
                    value if value.starts_with("checks failing") => {
                        "inspect the reported checks and fix or rerun them"
                    }
                    value if value.starts_with("already in ") => {
                        "select a PR whose changes are not already contained"
                    }
                    _ => continue,
                };
                if !steps.iter().any(|step| step == next) {
                    steps.push(next.to_string());
                }
            }
            if steps.is_empty() {
                steps.push("resolve the reported causes and run the plan again".to_string());
            }
            steps.join("; ")
        };
        let mut warnings = Vec::new();
        if self.mergeable_unknown || self.merge_state_status.eq_ignore_ascii_case("UNKNOWN") {
            warnings.push(
                "GitHub still computing; merge retries mergeability for up to a minute".to_string(),
            );
        }
        let mut exceptions = Vec::new();
        if reasons.is_empty() && self.state == PrState::Open && self.checks == Checks::Missing {
            exceptions.push(NO_CHECKS_EXCEPTION.to_string());
        }
        Readiness {
            id: self.key.to_string(),
            status: if reasons.is_empty() {
                "eligible"
            } else {
                "blocked"
            }
            .to_string(),
            causes: if reasons.is_empty() {
                vec!["preflight passed".to_string()]
            } else {
                reasons
            },
            next_step,
            base: self.base_ref.clone(),
            default_branch: self.default_branch.clone(),
            merge_state_status: self.merge_state_status.clone(),
            warnings,
            exceptions,
            checks: self.check_details.clone(),
        }
    }

    pub fn blockers(&self, allow_missing_checks: bool) -> Vec<String> {
        self.blockers_with(allow_missing_checks, true)
    }

    pub fn admin_override_reason(
        &self,
        allow_missing_checks: bool,
        require_approval: bool,
    ) -> Option<&'static str> {
        if self.state != PrState::Open
            || self.is_draft
            || self.review != Review::ReviewRequired
            || !matches!(self.checks, Checks::Success)
                && !(self.checks == Checks::Missing && allow_missing_checks)
            || self.conflicting
            || self.mergeable_unknown
            || self.behind
            || !self.merge_state_status.eq_ignore_ascii_case("BLOCKED")
        {
            return None;
        }
        let (blocked, _) = merge_state_blocker("BLOCKED")?;
        self.blockers_with(allow_missing_checks, require_approval)
            .iter()
            .all(|reason| {
                matches!(reason.as_str(), "review required" | "not approved") || reason == blocked
            })
            .then_some("REVIEW_REQUIRED")
    }

    pub fn blockers_with(&self, allow_missing_checks: bool, require_approval: bool) -> Vec<String> {
        let mut reasons = Vec::new();
        match self.state {
            PrState::Merged => {
                reasons.push("already merged".to_string());
                return reasons;
            }
            PrState::Closed => {
                reasons.push("closed".to_string());
                return reasons;
            }
            PrState::Open => {}
        }
        if self.is_draft {
            reasons.push("draft".to_string());
        }
        match self.review {
            Review::Approved => {}
            Review::ChangesRequested => reasons.push("changes requested".to_string()),
            Review::ReviewRequired if require_approval => {
                reasons.push("review required".to_string())
            }
            Review::None if require_approval => reasons.push("not approved".to_string()),
            Review::ReviewRequired | Review::None => {}
        }
        match self.checks {
            Checks::Success => {}
            Checks::Pending => reasons.push("checks pending".to_string()),
            Checks::Failure => reasons.push(self.failing_reason()),
            Checks::Missing => {
                if !allow_missing_checks {
                    reasons.push("no checks reported".to_string());
                }
            }
        }
        if self.conflicting {
            reasons.push("merge conflict".to_string());
        } else if self.behind {
            reasons.push("branch behind its base, update it".to_string());
        }
        if !(self.conflicting && self.merge_state_status.eq_ignore_ascii_case("BEHIND"))
            && let Some((cause, _)) = merge_state_blocker(&self.merge_state_status)
            && !reasons.iter().any(|reason| reason == cause)
        {
            reasons.push(cause.to_string());
        }
        reasons
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub pr: Option<String>,
    pub code: String,
    pub message: String,
    pub hint: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl Question {
    pub fn new(pr: Option<&PrKey>, code: &str, message: String, hint: &str) -> Self {
        Self {
            pr: pr.map(|k| k.to_string()),
            code: code.to_string(),
            message,
            hint: hint.to_string(),
            hidden: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_only() -> Pr {
        Pr {
            key: PrKey::new("acme", "api", 1),
            state: PrState::Open,
            title: String::new(),
            url: String::new(),
            author: String::new(),
            body: String::new(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            default_branch: "main".to_string(),
            merge_state_status: "BLOCKED".to_string(),
            check_details: Vec::new(),
            head_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            is_draft: false,
            review: Review::ReviewRequired,
            checks: Checks::Success,
            failed_checks: Vec::new(),
            conflicting: false,
            mergeable_unknown: false,
            behind: false,
            files: Vec::new(),
            files_complete: true,
            labels: Vec::new(),
        }
    }

    #[test]
    fn admin_override_requires_only_review_required_and_blocked() {
        let mut pr = review_only();
        for require_approval in [false, true] {
            assert_eq!(
                pr.admin_override_reason(false, require_approval),
                Some("REVIEW_REQUIRED")
            );
        }
        pr.merge_state_status = "bLoCkEd".to_string();
        assert_eq!(
            pr.admin_override_reason(false, true),
            Some("REVIEW_REQUIRED")
        );
        for state in [
            "CLEAN",
            "UNKNOWN",
            "BEHIND",
            "DIRTY",
            "DRAFT",
            "UNSTABLE",
            "HAS_HOOKS",
            "",
        ] {
            pr.merge_state_status = state.to_string();
            assert_eq!(pr.admin_override_reason(true, true), None, "{state}");
        }
    }

    #[test]
    fn admin_override_missing_checks_needs_existing_allowance() {
        let mut pr = review_only();
        pr.checks = Checks::Missing;
        assert_eq!(pr.admin_override_reason(false, true), None);
        assert_eq!(
            pr.admin_override_reason(true, true),
            Some("REVIEW_REQUIRED")
        );
        for checks in [Checks::Failure, Checks::Pending] {
            pr.checks = checks;
            assert_eq!(pr.admin_override_reason(true, true), None);
        }
    }

    #[test]
    fn admin_override_refuses_closed_draft_conflicting_behind_and_other_reviews() {
        for state in [PrState::Merged, PrState::Closed] {
            let mut pr = review_only();
            pr.state = state;
            assert_eq!(pr.admin_override_reason(true, true), None);
        }
        for review in [Review::Approved, Review::ChangesRequested, Review::None] {
            let mut pr = review_only();
            pr.review = review;
            assert_eq!(pr.admin_override_reason(true, false), None);
        }
        for field in ["draft", "conflict", "behind", "unknown"] {
            let mut pr = review_only();
            pr.is_draft = field == "draft";
            pr.conflicting = field == "conflict";
            pr.mergeable_unknown = field == "unknown";
            pr.behind = field == "behind";
            assert_eq!(pr.admin_override_reason(true, true), None);
        }
    }
}
