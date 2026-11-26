use super::{Fake, item, journal};
use crate::common::{Server, TempDir, serve_handler, serve_requests};
use anyhow::Result;
use pipetrain::command::merge::bump::{self, Gateway, GithubGateway, Options, Outcome};
use pipetrain::consumer_bump::backend::{
    GithubRemote, Invocation, LocalBackend, ProcessRunner, Runner,
};
use pipetrain::consumer_bump::journal::{Journal, Preparation, PublicationInput, Publisher};
use pipetrain::consumer_bump::{self, Consumer, PrepareResult};
use pipetrain::github::Client;
use pipetrain::merge::Store;
use pipetrain::planner::Item;
use pipetrain::publication::PublicationResult;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::Duration;

const CONSUMER: &str = "GET /repos/acme/app/pulls/2";
const REVIEW: &str = "GET /repos/acme/app/pulls/9";
const MANIFEST: &str = r#"<Project><ItemGroup><PackageReference Include="Acme.SDK" Version="1.2.3" /></ItemGroup></Project>"#;

struct LocalGit<'a>(&'a Path);

impl Runner for LocalGit<'_> {
    fn run(&mut self, invocation: &Invocation) -> Result<String> {
        assert_eq!(invocation.program, "git");
        let mut invocation = invocation.clone();
        for arg in &mut invocation.args {
            if arg == "https://github.com/acme/app.git" {
                *arg = self.0.to_str().unwrap().to_owned();
            }
        }
        invocation.args.splice(
            0..0,
            [
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "core.hooksPath=",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
            ]
            .map(str::to_owned),
        );
        ProcessRunner.run(&invocation)
    }
}

fn git(directory: &Path, args: &[&str]) -> String {
    LocalGit(directory)
        .run(&Invocation {
            program: "git".into(),
            directory: directory.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_secs(30),
        })
        .unwrap()
}

struct ConsumerGateway<'a> {
    github: GithubGateway<'a>,
    source: &'a Path,
}

impl Gateway for ConsumerGateway<'_> {
    fn publication(
        &mut self,
        publisher: &Publisher,
        package: &str,
        known: Option<&[PublicationInput]>,
    ) -> Result<Vec<PublicationResult>> {
        Fake::default().publication(publisher, package, known)
    }

    fn consumer(&mut self, item: &Item, packages: &[String]) -> Result<Consumer> {
        self.github.consumer(item, packages)
    }

    fn incorporated(&mut self, preparation: &Preparation, item: &Item) -> Result<bool> {
        self.github.incorporated(preparation, item)
    }

    fn prepare(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
    ) -> Result<PrepareResult> {
        let options = self.github.options;
        let mut backend = LocalBackend::new(
            options.workspace.clone().unwrap(),
            LocalGit(self.source),
            GithubRemote {
                client: self.github.client,
            },
            options.timeout,
        );
        consumer_bump::prepare_many(publications, consumer, &mut backend)
    }
}

fn save_initial(dir: &TempDir, journal: Journal) {
    let mut store = Store::open(Some(dir.join("state.json"))).unwrap();
    store.state.bump = Some(journal);
    store.save().unwrap();
}

fn client(server: &Server) -> Client {
    Client::with_base_url(
        "ghp_FAKE0123456789".into(),
        &Default::default(),
        false,
        &server.base,
    )
}

fn resume_saved(dir: &TempDir, server: &Server, item: &Item) -> Outcome {
    let mut store = Store::open(Some(dir.join("state.json"))).unwrap();
    let options = Options {
        workspace: Some(dir.join("workspaces")),
        ..Options::default()
    };
    bump::prepare(item, &mut client(server), &options, &mut store)
}

fn resume_with_local_publication(dir: &TempDir, server: &Server, item: &Item) -> Outcome {
    let mut store = Store::open(Some(dir.join("state.json"))).unwrap();
    let mut journal = store.state.bump.take().unwrap();
    let mut client = client(server);
    let workspace = dir.join("workspaces");
    let options = Options {
        workspace: Some(workspace.clone()),
        ..Options::default()
    };
    let source = dir.join("source");
    let mut gateway = ConsumerGateway {
        github: GithubGateway {
            client: &mut client,
            options: &options,
        },
        source: &source,
    };
    bump::prepare_with(
        item,
        &mut journal,
        &mut gateway,
        workspace.to_str().unwrap(),
        |updated| {
            store.state.bump = Some(updated.clone());
            store.save()
        },
    )
}

fn requests(server: &Server) -> Vec<String> {
    server
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| {
            s.lines()
                .next()
                .unwrap()
                .trim_end_matches(" HTTP/1.1")
                .to_owned()
        })
        .collect()
}

fn consumer_pr(sha: &str) -> Value {
    json!({"state":"open","head":{"sha":sha,"ref":"feature/app","repo":{"full_name":"Acme/App"}}})
}

fn saved_review(consumer: &Item, commit: &str) -> Journal {
    let mut state = journal();
    let mut original = Fake::default();
    let saved = original.marker_saved.clone();
    let result = bump::prepare_with(
        consumer,
        &mut state,
        &mut original,
        "previous-workspace",
        |j| {
            saved.set(j.consumers.contains_key(&consumer.id));
            Ok(())
        },
    );
    assert!(matches!(result, Outcome::NeedsReview { .. }));
    state
        .consumers
        .get_mut(&consumer.id)
        .unwrap()
        .prepared
        .as_mut()
        .unwrap()
        .commit = commit.into();
    state
}

#[test]
fn unexpected_consumer_identity_stops_before_creating_a_workspace_or_writing_to_github() {
    for (field, value) in [
        ("/head/sha", json!("0".repeat(40))),
        ("/head/ref", json!("other")),
        ("/head/repo/full_name", json!("fork/app")),
        ("/head/repo", Value::Null),
        ("/state", json!("closed")),
    ] {
        let dir = TempDir::new("consumer-guard");
        let mut pr = consumer_pr(&item().sha);
        *pr.pointer_mut(field).unwrap() = value;
        let server = serve_handler(move |path| {
            if path == CONSUMER {
                (200, pr.clone())
            } else {
                (404, Value::Null)
            }
        });
        save_initial(&dir, journal());
        let outcome = resume_with_local_publication(&dir, &server, &item());
        assert!(
            matches!(outcome, Outcome::Attention { reason } if reason.contains("consumer PR head changed")),
            "{field}"
        );
        assert_eq!(requests(&server), [CONSUMER], "{field}");
        assert!(!dir.join("workspaces").exists());
        let store = Store::open(Some(dir.join("state.json"))).unwrap();
        assert!(store.state.bump.unwrap().consumers.is_empty());
    }
}

#[test]
fn a_saved_review_stays_pending_until_its_exact_commit_is_merged_into_the_planned_head() {
    let commit = "2".repeat(40);
    let merge = "3".repeat(40);
    let compare = format!(
        "GET /repos/acme/app/compare/{merge}...{}?per_page=1",
        item().sha
    );
    for (merged, base, head, ancestry_checked) in [
        (false, "feature/app", commit.as_str(), false),
        (true, "other", commit.as_str(), false),
        (true, "feature/app", "unexpected", false),
        (true, "feature/app", commit.as_str(), true),
    ] {
        let dir = TempDir::new("pending-review");
        let pr = json!({"merged":merged,"base":{"ref":base},"head":{"sha":head},"merge_commit_sha":merge});
        let expected_compare = compare.clone();
        let server = serve_handler(move |path| {
            if path == REVIEW {
                (200, pr.clone())
            } else if path == expected_compare {
                (200, json!({"status":"diverged"}))
            } else {
                (404, Value::Null)
            }
        });
        let state = saved_review(&item(), &commit);
        let marker = state.consumers[&item().id].clone();
        save_initial(&dir, state);
        let before = fs::read(dir.join("state.json")).unwrap();
        for _ in 0..2 {
            let Outcome::NeedsReview {
                pull_request_url,
                branch,
                consumer_head,
            } = resume_saved(&dir, &server, &item())
            else {
                panic!("expected the original review")
            };
            assert_eq!(
                pull_request_url,
                marker.prepared.as_ref().unwrap().pull_request_url
            );
            assert_eq!(branch, marker.bump_branch);
            assert_eq!(consumer_head, marker.head_sha);
        }
        let expected = if ancestry_checked {
            vec![REVIEW, compare.as_str()]
        } else {
            vec![REVIEW]
        };
        assert_eq!(requests(&server), expected.repeat(2));
        assert_eq!(fs::read(dir.join("state.json")).unwrap(), before);
        assert!(!dir.join("workspaces").exists());
    }
}

#[test]
fn incorporated_bump_rechecks_the_pin_once_then_resumes_without_another_branch_or_pr() {
    let dir = TempDir::new("incorporated-review");
    let repo = dir.join("source");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "feature/app"]);
    git(&repo, &["commit", "--allow-empty", "-m", "consumer"]);
    let mut planned = item();
    planned.sha = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-b", "review"]);
    fs::write(repo.join("App.csproj"), MANIFEST).unwrap();
    fs::write(repo.join("App.sln"), "").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "pin Acme.SDK"]);
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "feature/app"]);
    git(
        &repo,
        &["merge", "--no-ff", "review", "-m", "incorporate bump"],
    );
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &["commit", "--allow-empty", "-m", "advance consumer"],
    );
    let mut consumer = item();
    consumer.sha = git(&repo, &["rev-parse", "HEAD"]);
    let refs = git(&repo, &["show-ref"]);
    let compare = format!(
        "GET /repos/acme/app/compare/{merge}...{}?per_page=1",
        consumer.sha
    );
    let tree = format!("GET /repos/acme/app/git/trees/{}?recursive=1", consumer.sha);
    let head = consumer.sha.clone();
    let review = json!({"merged":true,"base":{"ref":consumer.head},"head":{"sha":commit},"merge_commit_sha":merge});
    let server = serve_requests(move |path, body| {
        let value = match path {
            REVIEW => review.clone(),
            path if path == compare => json!({"status":"ahead"}),
            CONSUMER => consumer_pr(&head),
            "GET /repos/acme/app/pulls?state=open&base=feature/app&per_page=1" => json!([]),
            "GET /repos/acme/app" => json!({"default_branch":"main"}),
            path if path == tree => {
                json!({"tree":[{"type":"blob","path":"App.sln"},{"type":"blob","path":"App.csproj"}],"truncated":false})
            }
            "POST /graphql" => {
                let query: Value = serde_json::from_str(body).unwrap();
                if !query["query"].as_str().unwrap().starts_with("query(")
                    || query["variables"]["e0"] != format!("{head}:App.csproj")
                {
                    return (400, Value::Null);
                }
                json!({"data":{"repository":{"f0":{"text":MANIFEST}}}})
            }
            _ => return (404, Value::Null),
        };
        (200, value)
    });
    save_initial(&dir, saved_review(&planned, &commit));
    let outcome = resume_with_local_publication(&dir, &server, &consumer);
    assert!(matches!(outcome, Outcome::Skipped { .. }), "{outcome:?}");
    let state = Store::open(Some(dir.join("state.json")))
        .unwrap()
        .state
        .bump
        .unwrap();
    let marker = &state.consumers[&consumer.id];
    assert!(marker.completed && marker.prepared.is_none());
    assert_eq!(marker.head_sha, consumer.sha);
    let first = requests(&server);
    assert!(first.contains(&format!(
        "GET /repos/acme/app/compare/{merge}...{}?per_page=1",
        consumer.sha
    )));
    assert!(
        first
            .iter()
            .all(|req| req.starts_with("GET ") || req == "POST /graphql")
    );
    assert_eq!(fs::read_dir(dir.join("workspaces")).unwrap().count(), 1);
    let before = fs::read(dir.join("state.json")).unwrap();
    assert!(matches!(
        resume_saved(&dir, &server, &consumer),
        Outcome::Skipped { .. }
    ));
    assert_eq!(requests(&server), first);
    assert_eq!(fs::read(dir.join("state.json")).unwrap(), before);
    assert_eq!(fs::read_dir(dir.join("workspaces")).unwrap().count(), 1);
    assert_eq!(git(&repo, &["show-ref"]), refs);
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());
}
