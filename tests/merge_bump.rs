mod common;

use std::cell::Cell;
use std::rc::Rc;

use anyhow::{Result, bail};
use pipetrain::command::merge::bump::{self, Gateway, Outcome};
use pipetrain::consumer_bump::journal::{
    Contract, Journal, Preparation, PublicationInput, Publisher,
};
use pipetrain::consumer_bump::{Consumer, PrepareResult, PreparedBump};
use pipetrain::planner::Item;
use pipetrain::publication::{
    self, ExactFeed, PublicationResult, PublisherExecution, PublisherRun,
};

const SHA: &str = "1111111111111111111111111111111111111111";

fn item() -> Item {
    Item {
        id: "acme/app#2".into(),
        org: "acme".into(),
        repo: "app".into(),
        number: 2,
        title: "App".into(),
        url: "https://example.test/acme/app/pull/2".into(),
        author: String::new(),
        head: "feature/app".into(),
        base: "main".into(),
        sha: SHA.into(),
        layer: None,
        layer_rank: None,
        scope: String::new(),
        gate: None,
        gate_reason: None,
        manual_pause: false,
        gate_late: false,
        stage: 2,
        lane: None,
        after: Vec::new(),
        hidden: false,
    }
}

fn journal() -> Journal {
    let mut journal = Journal::default();
    journal.publishers.insert(
        "acme/sdk#1".into(),
        Publisher {
            repository: "acme/sdk".into(),
            org: "acme".into(),
            merge_sha: Some(SHA.into()),
            push_step: bump::DEFAULT_PUSH_STEP.into(),
            packages: vec!["Acme.SDK".into()],
            deploy_confirmed: true,
            publications: Default::default(),
        },
    );
    journal.contracts.insert(
        item().id,
        vec![Contract {
            publisher_id: "acme/sdk#1".into(),
            package: "Acme.SDK".into(),
        }],
    );
    journal
}

struct Feed;
impl ExactFeed for Feed {
    fn contains(&mut self, _: &str, version: &str) -> Result<bool> {
        Ok(matches!(version, "1.2.3" | "1.2.4"))
    }
}

#[derive(Default)]
struct Fake {
    skipped: bool,
    fail: bool,
    incorporated: bool,
    up_to_date: bool,
    generation: u64,
    publication_calls: usize,
    consumers: usize,
    preparations: usize,
    marker_saved: Rc<Cell<bool>>,
}

impl Gateway for Fake {
    fn publication(
        &mut self,
        publisher: &Publisher,
        package: &str,
        known: Option<&[PublicationInput]>,
    ) -> Result<Vec<PublicationResult>> {
        self.publication_calls += 1;
        if self.skipped {
            return Ok(vec![PublicationResult::Skipped {
                reason: "duplicate push".into(),
            }]);
        }
        let version = format!("1.2.{}", 3 + self.generation);
        if let Some(known) = known {
            assert_eq!(known[0].version, version);
            assert_eq!(known[0].run_id, 42 + self.generation);
        }
        let execution = PublisherExecution {
            repository: publisher.repository.clone(),
            merge_sha: publisher.merge_sha.clone().unwrap(),
            run_id: 42 + self.generation,
            published_this_execution: true,
        };
        let run = PublisherRun {
            repository: publisher.repository.clone(),
            head_sha: publisher.merge_sha.clone().unwrap(),
            id: 42 + self.generation,
            succeeded: true,
            push_step_succeeded: true,
            push_step_log: format!(
                "Pushing {package}.{version}.nupkg to feed\nYour package was pushed."
            ),
        };
        Ok(vec![publication::verify(
            &execution, &run, package, &mut Feed,
        )?])
    }
    fn consumer(&mut self, item: &Item, packages: &[String]) -> Result<Consumer> {
        self.consumers += 1;
        assert!(packages.contains(&"Acme.SDK".into()));
        Ok(Consumer {
            repository: "acme/app".into(),
            number: 2,
            head_branch: item.head.clone(),
            head_sha: item.sha.clone(),
            default_branch: "main".into(),
            bump_branch: format!("pipetrain/bump/2-nonce{}", self.preparations),
            solution: "App.sln".into(),
            manifests: vec!["App.csproj".into()],
        })
    }
    fn prepare(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
    ) -> Result<PrepareResult> {
        assert!(self.marker_saved.get());
        self.preparations += 1;
        if self.fail {
            bail!("remote operation uncertain")
        }
        if self.up_to_date {
            return Ok(PrepareResult::Skipped {
                reason: "already pins version".into(),
            });
        }
        let publications = publications
            .iter()
            .filter_map(|result| match result {
                PublicationResult::Published { evidence } => Some(evidence.clone()),
                _ => None,
            })
            .collect();
        Ok(PrepareResult::Prepared {
            bump: Box::new(PreparedBump {
                consumer_repository: consumer.repository.clone(),
                consumer_number: consumer.number,
                consumer_head: consumer.head_sha.clone(),
                branch: consumer.bump_branch.clone(),
                base: consumer.head_branch.clone(),
                commit: "2".repeat(40),
                pull_request_url: "https://github.com/acme/app/pull/9".into(),
                publications,
                needs_review: true,
            }),
        })
    }
    fn incorporated(&mut self, _: &Preparation, _: &Item) -> Result<bool> {
        Ok(self.incorporated)
    }
}

#[test]
fn legacy_or_missing_execution_proof_never_creates_a_workspace_or_pr() {
    let mut gateway = Fake::default();
    let result = bump::prepare_with(
        &item(),
        &mut Journal::default(),
        &mut gateway,
        "isolated",
        |_| Ok(()),
    );
    assert!(matches!(result, Outcome::Skipped { .. }));
    assert_eq!(
        gateway.publication_calls + gateway.consumers + gateway.preparations,
        0
    );
}

#[test]
fn duplicate_or_skipped_publication_is_a_normal_skip_without_consumer_operations() {
    let mut gateway = Fake {
        skipped: true,
        ..Default::default()
    };
    let result = bump::prepare_with(
        &item(),
        &mut journal(),
        &mut gateway,
        "isolated",
        |_| Ok(()),
    );
    assert!(matches!(result, Outcome::Skipped { .. }));
    assert_eq!(gateway.consumers + gateway.preparations, 0);
}

#[test]
fn new_publication_saves_inputs_and_pre_effect_marker_then_reuses_the_review_pr() {
    let mut journal = journal();
    let mut gateway = Fake::default();
    let saved = gateway.marker_saved.clone();
    let mut snapshots = Vec::new();
    let result = bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
        saved.set(journal.consumers.contains_key(&item().id));
        snapshots.push(serde_json::to_string(journal)?);
        Ok(())
    });
    assert!(matches!(result, Outcome::NeedsReview { .. }));
    assert_eq!(gateway.preparations, 1);
    let restored: Journal = serde_json::from_str(snapshots.last().unwrap()).unwrap();
    assert_eq!(
        restored.publishers["acme/sdk#1"].publications["acme.sdk"][0].run_id,
        42
    );
    let mut restored = restored;
    let again = bump::prepare_with(&item(), &mut restored, &mut gateway, "isolated", |_| Ok(()));
    assert!(matches!(again, Outcome::NeedsReview { .. }));
    assert_eq!(gateway.preparations, 1);
    gateway.incorporated = true;
    gateway.up_to_date = true;
    let mut changed = item();
    changed.sha = "3".repeat(40);
    assert!(matches!(
        bump::prepare_with(
            &changed,
            &mut restored,
            &mut gateway,
            "isolated",
            |_| Ok(())
        ),
        Outcome::Skipped { .. }
    ));
    assert_eq!(gateway.preparations, 2);
}

#[test]
fn partial_remote_operation_is_durable_attention_and_never_retried_blindly() {
    let mut journal = journal();
    let mut gateway = Fake {
        fail: true,
        ..Default::default()
    };
    let saved = gateway.marker_saved.clone();
    assert!(matches!(
        bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
            saved.set(journal.consumers.contains_key(&item().id));
            Ok(())
        }),
        Outcome::Attention { .. }
    ));
    assert!(!journal.consumers[&item().id].completed);
    let mut restored: Journal =
        serde_json::from_str(&serde_json::to_string(&journal).unwrap()).unwrap();
    gateway.fail = false;
    assert!(matches!(
        bump::prepare_with(&item(), &mut restored, &mut gateway, "isolated", |_| Ok(())),
        Outcome::Attention { .. }
    ));
    assert_eq!(gateway.preparations, 1);
}

#[test]
fn failed_marker_save_prevents_all_remote_preparation() {
    let mut journal = journal();
    let mut gateway = Fake::default();
    let result = bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
        if !journal.consumers.is_empty() {
            bail!("disk full")
        }
        Ok(())
    });
    assert!(matches!(result, Outcome::Attention { .. }));
    assert_eq!(gateway.preparations, 0);
}

#[test]
fn saved_inputs_are_reverified_when_publisher_is_missing_from_rebuilt_plan() {
    let mut journal = journal();
    journal
        .publishers
        .get_mut("acme/sdk#1")
        .unwrap()
        .publications
        .insert(
            "acme.sdk".into(),
            vec![PublicationInput {
                run_id: 42,
                version: "1.2.3".into(),
            }],
        );
    let mut gateway = Fake::default();
    let saved = gateway.marker_saved.clone();
    assert!(bump::applicable(&item(), Some(&journal)));
    assert!(matches!(
        bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
            saved.set(journal.consumers.contains_key(&item().id));
            Ok(())
        }),
        Outcome::NeedsReview { .. }
    ));
    assert_eq!(gateway.publication_calls, 1);
}

#[test]
fn newly_published_inputs_invalidate_completed_and_incorporated_preparations() {
    for up_to_date in [true, false] {
        let mut journal = journal();
        let mut gateway = Fake {
            up_to_date,
            ..Default::default()
        };
        let saved = gateway.marker_saved.clone();
        let first =
            bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
                saved.set(journal.consumers.contains_key(&item().id));
                Ok(())
            });
        assert!(matches!(
            first,
            Outcome::Skipped { .. } | Outcome::NeedsReview { .. }
        ));
        let mut publisher = journal.publishers["acme/sdk#1"].clone();
        publisher.merge_sha = Some("4".repeat(40));
        publisher.publications.clear();
        journal.publishers.insert("acme/sdk#33".into(), publisher);
        journal
            .contracts
            .get_mut(&item().id)
            .unwrap()
            .push(Contract {
                publisher_id: "acme/sdk#33".into(),
                package: "Acme.SDK".into(),
            });
        gateway.incorporated = true;
        gateway.up_to_date = false;
        gateway.generation = 1;
        let saved = gateway.marker_saved.clone();
        assert!(matches!(
            bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
                saved.set(journal.consumers.contains_key(&item().id));
                Ok(())
            }),
            Outcome::NeedsReview { .. }
        ));
        assert_eq!(gateway.preparations, 2);
        assert_eq!(gateway.publication_calls, 2);
    }
}

#[test]
fn incorporated_bump_with_reverted_pin_is_prepared_again_instead_of_cached_skip() {
    let mut journal = journal();
    let mut gateway = Fake::default();
    let saved = gateway.marker_saved.clone();
    assert!(matches!(
        bump::prepare_with(&item(), &mut journal, &mut gateway, "isolated", |journal| {
            saved.set(journal.consumers.contains_key(&item().id));
            Ok(())
        }),
        Outcome::NeedsReview { .. }
    ));
    let original_branch = journal.consumers[&item().id].bump_branch.clone();
    gateway.incorporated = true;
    let mut changed = item();
    changed.sha = "5".repeat(40);
    let saved = gateway.marker_saved.clone();
    assert!(matches!(
        bump::prepare_with(
            &changed,
            &mut journal,
            &mut gateway,
            "isolated",
            |journal| {
                saved.set(journal.consumers.contains_key(&item().id));
                Ok(())
            }
        ),
        Outcome::NeedsReview { .. }
    ));
    assert_eq!(gateway.preparations, 2);
    assert_ne!(journal.consumers[&item().id].bump_branch, original_branch);
    assert_eq!(journal.consumers[&item().id].head_sha, changed.sha);
}
