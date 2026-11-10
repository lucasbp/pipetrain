use anyhow::{Result, bail};
use pipetrain::consumer_bump::{self, BumpGateway, Consumer, PrepareResult};
use pipetrain::publication::{
    self, ExactFeed, PublicationResult, PublisherExecution, PublisherRun,
};
use std::collections::BTreeMap;

const SHA: &str = "1111111111111111111111111111111111111111";
const VERSION: &str = "1.2.3.4";

#[derive(Default)]
struct Feed {
    calls: Vec<(String, String)>,
    published: bool,
}
impl ExactFeed for Feed {
    fn contains(&mut self, package: &str, version: &str) -> Result<bool> {
        self.calls.push((package.into(), version.into()));
        Ok(self.published)
    }
}
fn execution() -> PublisherExecution {
    PublisherExecution {
        repository: "acme/library".into(),
        merge_sha: SHA.into(),
        run_id: 42,
        published_this_execution: true,
    }
}
fn run(log: &str) -> PublisherRun {
    PublisherRun {
        repository: "acme/library".into(),
        head_sha: SHA.into(),
        id: 42,
        succeeded: true,
        push_step_succeeded: true,
        push_step_log: log.into(),
    }
}
fn log() -> &'static str {
    "Pushing Acme.Util.1.2.3.4.nupkg to 'feed'...\nYour package was pushed.\n"
}
fn proof() -> PublicationResult {
    publication::verify(
        &execution(),
        &run(log()),
        "Acme.Util",
        &mut Feed {
            published: true,
            ..Feed::default()
        },
    )
    .unwrap()
}
#[test]
fn proves_first_new_push_and_exact_feed_version() {
    let mut feed = Feed {
        published: true,
        ..Feed::default()
    };
    let PublicationResult::Published { evidence } =
        publication::verify(&execution(), &run(log()), "Acme.Util", &mut feed).unwrap()
    else {
        panic!()
    };
    assert_eq!(evidence.version(), VERSION);
    assert_eq!(evidence.run_id(), 42);
    assert_eq!(feed.calls, [("Acme.Util".into(), VERSION.into())]);
}
#[test]
fn duplicate_other_package_does_not_invalidate_new_target_push() {
    let mixed = format!(
        "{}Pushing Acme.Other.1.0.0.nupkg to 'feed'...\nConflict: already exists, skipping.\n",
        log()
    );
    assert!(matches!(
        publication::verify(
            &execution(),
            &run(&mixed),
            "Acme.Util",
            &mut Feed {
                published: true,
                ..Feed::default()
            }
        )
        .unwrap(),
        PublicationResult::Published { .. }
    ));
}
#[test]
fn target_duplicate_does_not_borrow_other_packages_success() {
    let mixed = "Pushing Acme.Util.1.2.3.4.nupkg to 'feed'...\nConflict: already exists, skipping.\nPushing Acme.Other.2.0.0.nupkg to 'feed'...\nYour package was pushed.";
    let mut feed = Feed::default();
    assert!(matches!(
        publication::verify(&execution(), &run(mixed), "Acme.Util", &mut feed).unwrap(),
        PublicationResult::Skipped { .. }
    ));
    assert!(feed.calls.is_empty());
}
#[test]
fn skipped_publisher_cannot_use_old_feed_version_even_if_forced_by_caller() {
    let mut request = execution();
    request.published_this_execution = false;
    let mut feed = Feed {
        published: true,
        ..Feed::default()
    };
    assert!(matches!(
        publication::verify(&request, &run(log()), "Acme.Util", &mut feed).unwrap(),
        PublicationResult::Skipped { .. }
    ));
    assert!(feed.calls.is_empty());
}

#[test]
fn missing_or_incomplete_log_evidence_is_an_error_not_an_old_version_bump() {
    for log in [
        "",
        "Pushing Acme.Util.1.2.3.4.nupkg to 'feed'...",
        "unrecognized publisher output",
    ] {
        let mut feed = Feed {
            published: true,
            ..Feed::default()
        };
        assert!(publication::verify(&execution(), &run(log), "Acme.Util", &mut feed).is_err());
        assert!(feed.calls.is_empty());
    }
}
#[test]
fn wrong_sha_run_and_feed_are_rejected() {
    for (sha, id) in [("2222222222222222222222222222222222222222", 42), (SHA, 43)] {
        let mut workflow = run(log());
        workflow.head_sha = sha.into();
        workflow.id = id;
        assert!(
            publication::verify(
                &execution(),
                &workflow,
                "Acme.Util",
                &mut Feed {
                    published: true,
                    ..Feed::default()
                }
            )
            .is_err()
        );
    }
    assert!(
        publication::verify(&execution(), &run(log()), "Acme.Util", &mut Feed::default()).is_err()
    );
}
fn consumer() -> Consumer {
    Consumer {
        repository: "acme/api".into(),
        number: 7,
        head_branch: "feature/consumer".into(),
        head_sha: SHA.into(),
        default_branch: "main".into(),
        bump_branch: "pipetrain/bump/unique-42".into(),
        solution: "src/App.sln".into(),
        manifests: vec!["src/App.csproj".into()],
    }
}
fn manifest() -> String {
    "<Project><ItemGroup><PackageReference Include=\"Acme.Util\" Version=\"1.2.0\" /></ItemGroup></Project>".into()
}
#[derive(Default)]
struct Gateway {
    files: BTreeMap<String, String>,
    events: Vec<String>,
    move_at: Option<usize>,
    checks: usize,
    restore_fails: bool,
    wrong_lock: bool,
    foreign_change: bool,
}
impl Gateway {
    fn new() -> Self {
        Self {
            files: BTreeMap::from([("src/App.csproj".into(), manifest())]),
            ..Self::default()
        }
    }
}
impl BumpGateway for Gateway {
    fn consumer_head(&mut self, _: &Consumer) -> Result<String> {
        self.checks += 1;
        Ok(if self.move_at == Some(self.checks) {
            "moved"
        } else {
            SHA
        }
        .into())
    }
    fn create_worktree(&mut self, _: &Consumer) -> Result<()> {
        self.events.push("worktree".into());
        Ok(())
    }
    fn read(&mut self, path: &str) -> Result<String> {
        Ok(self.files[path].clone())
    }
    fn write(&mut self, path: &str, text: &str) -> Result<()> {
        self.events.push("write".into());
        self.files.insert(path.into(), text.into());
        Ok(())
    }
    fn restore(&mut self, _: &str, locked: bool) -> Result<()> {
        self.events.push(format!("restore:{locked}"));
        if self.restore_fails {
            bail!("restore failed");
        }
        let version = if self.wrong_lock { "0.0.0" } else { VERSION };
        self.files.insert(
            "src/packages.lock.json".into(),
                serde_json::json!({"dependencies":{"net8.0":{"Acme.Util":{"resolved":version},"Acme.SDK":{"resolved":version}}}})
                .to_string(),
        );
        Ok(())
    }
    fn changed_files(&mut self) -> Result<Vec<String>> {
        if !self.events.iter().any(|e| e == "write") {
            return Ok(vec![]);
        }
        let mut paths = vec!["src/App.csproj".into(), "src/packages.lock.json".into()];
        if self.foreign_change {
            paths.push("src/Program.cs".into());
        }
        Ok(paths)
    }
    fn commit(&mut self, _: &[String], _: &str) -> Result<String> {
        self.events.push("commit".into());
        Ok(SHA.into())
    }
    fn push_new_branch(&mut self, _: &str, _: &str) -> Result<()> {
        self.events.push("push".into());
        Ok(())
    }
    fn open_review_pr(&mut self, request: &Consumer, _: &str, body: &str) -> Result<String> {
        assert_eq!(request.head_branch, "feature/consumer");
        assert!(body.contains("Requires new CI and human review"));
        self.events.push("pr".into());
        Ok("https://github.com/acme/api/pull/8".into())
    }
}

fn additional_proof(package: &str, version: &str) -> PublicationResult {
    let log = format!("Pushing {package}.{version}.nupkg to 'feed'...\nYour package was pushed.");
    publication::verify(
        &execution(),
        &run(&log),
        package,
        &mut Feed {
            published: true,
            ..Feed::default()
        },
    )
    .unwrap()
}

#[test]
fn groups_multiple_published_dependencies_into_one_workspace_restore_and_review_pr() {
    let mut gateway = Gateway::new();
    gateway.files.insert("src/App.csproj".into(), "<Project><PackageReference Include=\"Acme.Util\" Version=\"1.0.0\" /><PackageReference Include=\"Acme.SDK\" Version=\"1.0.0\" /></Project>".into());
    let publications = [
        proof(),
        additional_proof("Acme.SDK", VERSION),
        PublicationResult::Skipped {
            reason: "another publisher skipped".into(),
        },
    ];
    let PrepareResult::Prepared { bump } =
        consumer_bump::prepare_many(&publications, &consumer(), &mut gateway).unwrap()
    else {
        panic!()
    };
    assert_eq!(bump.publications.len(), 2);
    assert_eq!(
        gateway.events,
        [
            "worktree",
            "write",
            "restore:false",
            "restore:true",
            "commit",
            "push",
            "pr"
        ]
    );
    assert_eq!(gateway.files["src/App.csproj"].matches(VERSION).count(), 2);
}

#[test]
fn conflicting_evidence_is_rejected_before_touching_the_consumer() {
    let mut gateway = Gateway::new();
    assert!(
        consumer_bump::prepare_many(
            &[proof(), additional_proof("Acme.Util", "2.0.0")],
            &consumer(),
            &mut gateway
        )
        .is_err()
    );
    assert!(gateway.events.is_empty());
    assert_eq!(gateway.checks, 0);
}

#[test]
fn already_pinned_consumer_does_not_restore_commit_or_open_a_review() {
    let mut gateway = Gateway::new();
    gateway.files.insert(
        "src/App.csproj".into(),
        format!("<PackageReference Include=\"Acme.Util\" Version=\"{VERSION}\" />"),
    );
    assert!(matches!(
        consumer_bump::prepare_many(&[proof(), proof()], &consumer(), &mut gateway).unwrap(),
        PrepareResult::Skipped { .. }
    ));
    assert_eq!(gateway.events, ["worktree"]);
}
#[test]
fn prepares_exact_pin_and_locked_restore_then_review_pr_against_consumer_head() {
    let mut gateway = Gateway::new();
    let PrepareResult::Prepared { bump } =
        consumer_bump::prepare(&proof(), &consumer(), &mut gateway).unwrap()
    else {
        panic!()
    };
    assert!(gateway.files["src/App.csproj"].contains(VERSION));
    assert_eq!(
        gateway.events,
        [
            "worktree",
            "write",
            "restore:false",
            "restore:true",
            "commit",
            "push",
            "pr"
        ]
    );
    assert!(bump.needs_review);
    assert_eq!(bump.base, "feature/consumer");
}
#[test]
fn skipped_publication_causes_no_workspace_or_remote_mutations() {
    let mut gateway = Gateway::new();
    assert!(matches!(
        consumer_bump::prepare(
            &PublicationResult::Skipped {
                reason: "publisher skipped".into()
            },
            &consumer(),
            &mut gateway
        )
        .unwrap(),
        PrepareResult::Skipped { .. }
    ));
    assert!(gateway.events.is_empty());
    assert_eq!(gateway.checks, 0);
}
#[test]
fn refuses_moved_head_at_each_mutation_boundary() {
    for check in 1..=4 {
        let mut gateway = Gateway {
            move_at: Some(check),
            ..Gateway::new()
        };
        assert!(consumer_bump::prepare(&proof(), &consumer(), &mut gateway).is_err());
        assert!(!gateway.events.iter().any(|e| e == "pr"));
        if check <= 3 {
            assert!(!gateway.events.iter().any(|e| e == "push"));
        }
    }
}
#[test]
fn restore_failure_wrong_lock_and_foreign_changes_never_push_partial_work() {
    for mode in 0..3 {
        let mut gateway = Gateway {
            restore_fails: mode == 0,
            wrong_lock: mode == 1,
            foreign_change: mode == 2,
            ..Gateway::new()
        };
        assert!(consumer_bump::prepare(&proof(), &consumer(), &mut gateway).is_err());
        assert!(
            !gateway
                .events
                .iter()
                .any(|e| e == "commit" || e == "push" || e == "pr")
        );
        assert!(gateway.files["src/App.csproj"].contains(VERSION));
    }
}
#[test]
fn no_default_branch_or_unsafe_paths() {
    for (branch, path) in [
        ("main", "src/App.csproj"),
        ("feature/consumer", "../escape.csproj"),
    ] {
        let mut request = consumer();
        request.head_branch = branch.into();
        request.manifests = vec![path.into()];
        assert!(consumer_bump::prepare(&proof(), &request, &mut Gateway::new()).is_err());
    }
}
#[test]
fn pins_literal_and_central_versions_but_refuses_downgrade_properties_and_conditions() {
    for text in [
        "<PackageReference Include=\"Acme.Util\" Version=\"9.0.0\" />",
        "<PackageReference Include=\"Acme.Util\" Version=\"$(Shared)\" />",
        "<ItemGroup Condition =\"true\"><PackageReference Include=\"Acme.Util\" Version=\"1.0.0\" /></ItemGroup>",
    ] {
        assert!(consumer_bump::pin_manifest(text, "Acme.Util", VERSION).is_err());
    }
    let central = "<PackageVersion Include=\"Acme.Util\" Version=\"1.0.0\" />";
    assert!(
        consumer_bump::pin_manifest(central, "Acme.Util", VERSION)
            .unwrap()
            .contains(VERSION)
    );
    assert!(
        consumer_bump::pin_manifest(
            "<!-- <PackageReference Include=\"Acme.Util\" Version=\"1.0.0\" /> -->",
            "Acme.Util",
            VERSION
        )
        .is_err()
    );
}
