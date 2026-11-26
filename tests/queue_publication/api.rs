use crate::common::git::git_in;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

const WORKFLOW: &str = ".github/workflows/publish.yml";
const MERGED: &str = "1111111111111111111111111111111111111111";
const STAMP: &str = "2026-11-01T12:00:00Z";

pub struct Github {
    pub repo: PathBuf,
    pub changed_pin: bool,
    pub merged: BTreeSet<u64>,
    pub reviews: Vec<Value>,
    pub second_base: &'static str,
    pub deploy_conclusion: &'static str,
    pub hide_pushed_ref: bool,
    pub foreign_feed: bool,
    pub conflicting_update: Option<u64>,
    pub checks: &'static str,
    pub hold_updated_heads: bool,
    pub readiness: BTreeMap<String, (&'static str, &'static str)>,
    pub seen_readiness: BTreeSet<(u64, String, &'static str, &'static str)>,
    pub reported_checks_sha: BTreeMap<u64, String>,
    pub review_reads: Vec<Instant>,
    pub open: BTreeMap<u64, &'static str>,
    pub dependencies: BTreeMap<u64, &'static str>,
    pub writes: Vec<String>,
    pub unexpected: Vec<String>,
    pub lose_create_reply: bool,
    pub feed_has_version: bool,
    evidence: bool,
}

impl Github {
    pub fn new(repo: PathBuf, evidence: bool) -> Self {
        Self {
            repo,
            changed_pin: false,
            merged: Default::default(),
            reviews: vec![],
            second_base: "main",
            deploy_conclusion: "success",
            hide_pushed_ref: false,
            foreign_feed: false,
            conflicting_update: None,
            checks: "SUCCESS",
            hold_updated_heads: false,
            readiness: BTreeMap::new(),
            seen_readiness: BTreeSet::new(),
            reported_checks_sha: BTreeMap::new(),
            review_reads: Vec::new(),
            open: BTreeMap::new(),
            dependencies: BTreeMap::new(),
            writes: vec![],
            unexpected: vec![],
            lose_create_reply: false,
            feed_has_version: evidence,
            evidence,
        }
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.repo, args)
    }

    fn git_optional(&self, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_string())
    }

    pub fn head(&self, number: u64) -> String {
        if number >= 99 {
            self.reviews[(number - 99) as usize]["head"]["sha"]
                .as_str()
                .unwrap()
                .to_owned()
        } else if [2, 3].contains(&number) {
            self.git(&["rev-parse", &format!("feature/{number}")])
        } else {
            format!("{number:040x}")
        }
    }
    fn base(&self, number: u64) -> &'static str {
        if number == 3 {
            self.second_base
        } else {
            "main"
        }
    }

    pub fn integrate(&mut self) {
        let review = &mut self.reviews[0];
        let sha = review["head"]["sha"].as_str().unwrap().to_string();
        review["merged"] = json!(true);
        review["state"] = json!("closed");
        review["merged_at"] = json!(STAMP);
        review["merge_commit_sha"] = json!(sha);
        let branch = format!("refs/heads/{}", review["base"]["ref"].as_str().unwrap());
        self.git(&["update-ref", &branch, &sha]);
    }

    pub fn incorporate(&self, number: u64) {
        let head = self.head(number);
        let branch = format!("refs/heads/feature/{number}");
        let base = self.git(&["rev-parse", self.base(number)]);
        let tree = self.git(&["merge-tree", "--write-tree", &head, &base]);
        let commit = self.git(&[
            "commit-tree",
            &tree,
            "-p",
            &head,
            "-p",
            &base,
            "-m",
            "Update branch",
        ]);
        self.git(&["update-ref", &branch, &commit, &head]);
    }

    fn update(&mut self, number: u64, body: &Value) -> (u16, Value) {
        assert_eq!(body["expected_head_sha"], self.head(number));
        if self.conflicting_update == Some(number) {
            return (
                422,
                json!({"message":"Merge conflict updating the feature branch"}),
            );
        }
        self.incorporate(number);
        if self.hold_updated_heads {
            self.readiness
                .insert(self.head(number), ("PENDING", "REVIEW_REQUIRED"));
        }
        (202, json!({"message":"Updating"}))
    }

    fn pr_node(&mut self, repo: &str, number: u64) -> Value {
        let head = self.head(number);
        let (checks, review) = self.readiness.get(&head).copied().unwrap_or((
            if repo == "web" {
                self.checks
            } else {
                "SUCCESS"
            },
            "APPROVED",
        ));
        let check_commit = self.reported_checks_sha.get(&number).unwrap_or(&head);
        self.seen_readiness
            .insert((number, check_commit.clone(), checks, review));
        let branch = if number >= 99 {
            self.reviews[(number - 99) as usize]["head"]["ref"]
                .as_str()
                .unwrap()
                .to_owned()
        } else {
            format!("feature/{number}")
        };
        json!({"number":number,"title":format!("Feature {number}"),"body":self.dependencies.get(&number).unwrap_or(&""),"url":format!("https://github.com/acme/{repo}/pull/{number}"),"state":if self.merged.contains(&number) {"MERGED"} else {"OPEN"},
            "headRefName":branch,"baseRefName":self.base(number),"headRefOid":head,"baseRef":{"target":{"oid":if repo=="web"{self.git(&["rev-parse",self.base(number)])}else{"b".repeat(40)}}},
            "isDraft":false,"mergeable":"MERGEABLE","reviewDecision":review,"mergeStateStatus":"CLEAN","latestOpinionatedReviews":{"nodes":if review=="APPROVED"{vec![json!({"state":"APPROVED"})]}else{vec![]}},
            "repository":{"name":repo,"owner":{"login":"acme"},"defaultBranchRef":{"name":"main"}},
            "commits":{"nodes":[{"commit":{"oid":check_commit,"statusCheckRollup":{"state":checks,"contexts":{"nodes":[]}}}}]}})
    }

    fn tree(&self, repo: &str, sha: &str) -> Vec<String> {
        match repo {
            "sdk" => vec!["Core.csproj".into(), WORKFLOW.into()],
            "broken" => vec![WORKFLOW.into()],
            "web" => self
                .git(&["ls-tree", "-r", "--name-only", sha])
                .lines()
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn blob(&self, repo: &str, expression: &str) -> Option<String> {
        let (_, path) = expression.split_once(':')?;
        match (repo,path) {
            ("sdk","Core.csproj")=>Some("<Project><PropertyGroup><PackageId>Acme.Core</PackageId></PropertyGroup></Project>".into()),
            ("sdk" | "broken", WORKFLOW)=>Some("on: push\njobs:\n  publish:\n    runs-on: ubuntu-latest\n    steps:\n      - run: dotnet nuget push\n".into()),
            ("web",_)=>self.git_optional(&["show",expression]),
            _=>None,
        }
    }

    fn graphql(&mut self, body: &Value) -> Value {
        let vars = &body["variables"];
        if vars["q"]
            .as_str()
            .is_some_and(|query| query.starts_with("org:acme "))
        {
            let open: Vec<_> = self
                .open
                .iter()
                .filter(|(number, _)| !self.merged.contains(number))
                .map(|(n, repo)| (*n, *repo))
                .collect();
            let nodes: Vec<_> = open
                .into_iter()
                .map(|(n, repo)| self.pr_node(repo, n))
                .collect();
            return json!({"data":{"search":{"issueCount":nodes.len(),"nodes":nodes,"pageInfo":{"hasNextPage":false}}}});
        }
        let repo = vars["r"].as_str().unwrap_or_default();
        if let Some(n) = vars["n"].as_u64() {
            return json!({"data":{"repository":{"pullRequest":self.pr_node(repo,n)}}});
        }
        let mut fields = Map::new();
        for (key, value) in vars.as_object().unwrap() {
            if let Some(index) = key.strip_prefix('e')
                && let Some(expr) = value.as_str()
            {
                fields.insert(
                    format!("f{index}"),
                    self.blob(repo, expr)
                        .map_or(Value::Null, |text| json!({"text":text})),
                );
            }
        }
        json!({"data":{"repository":fields,"search":{"nodes":[],"issueCount":0,"pageInfo":{"hasNextPage":false}}}})
    }

    fn run(&self) -> Value {
        json!({"id":70,"workflow_id":60,"name":"publish","path":WORKFLOW,"event":"push","repository":{"full_name":"acme/sdk"},
            "head_sha":MERGED,"status":"completed","conclusion":self.deploy_conclusion,"html_url":"https://github.com/acme/sdk/actions/runs/70"})
    }

    pub fn push_log(&self, host: &str) -> String {
        let owner = if self.foreign_feed { "another" } else { "acme" };
        let push =
            format!("{STAMP} Pushing Acme.Core.1.1.0.nupkg to 'http://{host}/packages/{owner}'...");
        if self.evidence {
            format!("{push}\n{STAMP} Your package was pushed.\n")
        } else {
            format!("{push}\n{STAMP} Conflict 409 duplicate package\n")
        }
    }

    fn pull(&self, repo: &str, number: u64) -> Value {
        if number >= 99 {
            return self
                .reviews
                .get((number - 99) as usize)
                .cloned()
                .unwrap_or(Value::Null);
        }
        let full = format!("acme/{repo}");
        let merged = self.merged.contains(&number);
        json!({"number":number,"state":if merged{"closed"}else{"open"},"merged":merged,"changed_files":1,
            "base":{"ref":self.base(number),"repo":{"full_name":full}},"head":{"sha":self.head(number),"ref":format!("feature/{number}"),"repo":{"full_name":full}},
            "html_url":format!("https://github.com/{full}/pull/{number}"),"mergeable_state":"clean","merge_commit_sha":MERGED})
    }

    pub fn route(&mut self, line: &str, body: &str) -> (u16, Value) {
        let (method, url) = line.split_once(' ').unwrap();
        let body = serde_json::from_str(body).unwrap_or(Value::Null);
        if line == "POST /graphql" {
            return (200, self.graphql(&body));
        }
        let path = url
            .split('?')
            .next()
            .unwrap()
            .replace("%2F", "/")
            .replace("%2f", "/");
        let Some(rest) = path.strip_prefix("/repos/acme/") else {
            return (404, Value::Null);
        };
        let (repo, route) = rest.split_once('/').unwrap_or((rest, ""));
        if method != "GET" {
            self.writes.push(line.into());
        }
        match (method, route) {
            ("GET", "") => (200, json!({"default_branch":"main"})),
            ("GET", "pulls") if url.contains("state=all") => {
                let found: Vec<_> = self
                    .reviews
                    .iter()
                    .filter(|pr| {
                        let head = pr["head"]["ref"].as_str().unwrap();
                        let base = pr["base"]["ref"].as_str().unwrap();
                        url.contains(&format!("head=acme%3A{head}&base={base}&"))
                    })
                    .collect();
                (200, json!(found))
            }
            ("GET", "pulls") => (200, json!([])),
            ("GET", "actions/runs") => {
                let mut run = self.run();
                if repo == "broken" {
                    run["conclusion"] = "failure".into();
                    run["repository"]["full_name"] = "acme/broken".into();
                }
                (200, json!({"total_count":1,"workflow_runs":[run]}))
            }
            ("GET", "actions/runs/70") => (200, self.run()),
            ("GET", "actions/runs/70/jobs") => (
                200,
                json!({"total_count":1,"jobs":[{"id":80,"conclusion":"success","steps":[{"name":"Push to GitHub Packages","conclusion":"success","started_at":STAMP,"completed_at":STAMP}]}]}),
            ),
            ("GET", route) if route.starts_with("git/trees/") => {
                let files: Vec<_> = self
                    .tree(repo, &route["git/trees/".len()..])
                    .into_iter()
                    .map(|path| json!({"path":path,"type":"blob"}))
                    .collect();
                (200, json!({"truncated":false,"tree":files}))
            }
            ("GET", route) if route.starts_with("git/ref/heads/") => {
                let reference = &route["git/ref/heads/".len()..];
                if repo != "web" {
                    return (200, json!({"object":{"sha":MERGED}}));
                }
                let Some(sha) = self.git_optional(&[
                    "rev-parse",
                    "--verify",
                    &format!("refs/heads/{reference}"),
                ]) else {
                    return (404, Value::Null);
                };
                if self.hide_pushed_ref && reference.starts_with("pipetrain/bump/") {
                    self.hide_pushed_ref = false;
                    return (
                        503,
                        json!({"message":"Branch check temporarily unavailable after push"}),
                    );
                }
                (200, json!({"object":{"sha":sha}}))
            }
            ("GET", route) if repo == "web" && route.starts_with("git/commits/") => {
                let sha = route.strip_prefix("git/commits/").unwrap();
                let parents: Vec<_> = self
                    .git(&["show", "-s", "--format=%P", sha])
                    .split_whitespace()
                    .map(|sha| json!({"sha":sha}))
                    .collect();
                (200, json!({"sha":sha,"parents":parents}))
            }
            ("GET", route) if route.starts_with("compare/") => {
                let pair = &route["compare/".len()..];
                let (base, head) = pair.split_once("...").unwrap();
                let status = if repo != "web" {
                    "ahead"
                } else if base == head {
                    "identical"
                } else if self
                    .git_optional(&["merge-base", "--is-ancestor", base, head])
                    .is_some()
                {
                    "ahead"
                } else {
                    "diverged"
                };
                (
                    200,
                    json!({"status":status,"ahead_by":usize::from(status=="ahead"),"behind_by":usize::from(status=="diverged")}),
                )
            }
            ("POST", "git/refs") => {
                let reference = body["ref"].as_str().unwrap();
                self.git(&[
                    "update-ref",
                    reference,
                    body["sha"].as_str().unwrap(),
                    &"0".repeat(40),
                ]);
                (201, json!({"ref":reference}))
            }
            ("POST", "pulls") => {
                assert!(
                    self.reviews
                        .iter()
                        .all(|pull| pull["head"]["ref"] != body["head"])
                );
                assert_eq!(body["draft"], true);
                let head = body["head"].as_str().unwrap();
                let sha = self.git(&["rev-parse", head]);
                let number = 99 + self.reviews.len();
                self.reviews.push(
                    json!({"number":number,"state":"open","draft":true,"merged":false,"merged_at":null,"html_url":format!("https://github.com/acme/web/pull/{number}"),"title":body["title"],"body":body["body"],
                    "base":{"ref":body["base"],"repo":{"full_name":"acme/web"}},"head":{"ref":head,"sha":sha,"repo":{"full_name":"acme/web"}}}),
                );
                if self.lose_create_reply {
                    (502, json!({"message":"Gateway failed after creation"}))
                } else {
                    (201, self.reviews.last().unwrap().clone())
                }
            }
            (_, route) if route.starts_with("pulls/") => {
                let mut parts = route.split('/');
                let number = parts.nth(1).unwrap().parse::<u64>().unwrap();
                match (method, parts.next()) {
                    ("GET", None) => {
                        if repo == "web" && number == 99 {
                            self.review_reads.push(Instant::now());
                        }
                        (200, self.pull(repo, number))
                    }
                    ("GET", Some("files")) if repo == "web" && number == 2 && self.changed_pin => {
                        let patch = self.git(&["diff", "main...feature/2", "--", "Web.csproj"]);
                        (
                            200,
                            json!([{"filename":"Web.csproj","status":"modified","patch":patch}]),
                        )
                    }
                    ("GET", Some("files")) => (
                        200,
                        json!([{"filename":format!("Feature{number}.cs"),"status":"added"}]),
                    ),
                    ("PUT", Some("merge")) => {
                        assert!(
                            number < 99 && self.merged.insert(number),
                            "repeated or disallowed merge of #{number}"
                        );
                        assert_eq!(body["sha"], self.head(number));
                        (200, json!({"merged":true,"sha":MERGED}))
                    }
                    ("PUT", Some("update-branch")) => self.update(number, &body),
                    _ => panic!("unexpected pull operation {line}"),
                }
            }
            _ => {
                self.unexpected.push(line.into());
                (404, json!({"message":"Unknown fixture route"}))
            }
        }
    }
}
