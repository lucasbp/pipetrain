use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Journal {
    pub publishers: BTreeMap<String, Publisher>,
    pub contracts: BTreeMap<String, Vec<Contract>>,
    pub consumers: BTreeMap<String, Preparation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Publisher {
    pub repository: String,
    pub org: String,
    pub merge_sha: Option<String>,
    pub push_step: String,
    pub packages: Vec<String>,
    pub deploy_confirmed: bool,
    pub publications: BTreeMap<String, Vec<PublicationInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationInput {
    pub run_id: u64,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preparation {
    pub repository: String,
    pub number: u64,
    pub head_branch: String,
    pub head_sha: String,
    pub bump_branch: String,
    pub workspace_root: String,
    pub completed: bool,
    #[serde(default)]
    pub publication_fingerprint: String,
    pub prepared: Option<Prepared>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub commit: String,
    pub pull_request_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub publisher_id: String,
    pub package: String,
}
