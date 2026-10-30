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
    pub body: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_sha: String,
    pub is_draft: bool,
    pub review: Review,
    pub checks: Checks,
    pub conflicting: bool,
    pub mergeable_unknown: bool,
    pub files: Vec<ChangedFile>,
    pub files_complete: bool,
}

impl Pr {
    pub fn blockers(&self, allow_missing_checks: bool) -> Vec<String> {
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
            Review::ReviewRequired => reasons.push("review required".to_string()),
            Review::None => reasons.push("not approved".to_string()),
        }
        match self.checks {
            Checks::Success => {}
            Checks::Pending => reasons.push("checks pending".to_string()),
            Checks::Failure => reasons.push("checks failing".to_string()),
            Checks::Missing => {
                if !allow_missing_checks {
                    reasons.push("no checks reported".to_string());
                }
            }
        }
        if self.conflicting {
            reasons.push("merge conflict".to_string());
        } else if self.mergeable_unknown {
            reasons.push("mergeability unknown, retry in a minute".to_string());
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
}

impl Question {
    pub fn new(pr: Option<&PrKey>, code: &str, message: String, hint: &str) -> Self {
        Self {
            pr: pr.map(|k| k.to_string()),
            code: code.to_string(),
            message,
            hint: hint.to_string(),
        }
    }
}
