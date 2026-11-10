#[path = "../src/merge_scheduler.rs"]
mod merge_scheduler;

use merge_scheduler::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Recorder {
    states: Arc<Mutex<BTreeMap<String, Status>>>,
    confirmations: Vec<String>,
    progress: Vec<String>,
    fail_persist: Option<String>,
    fail_result: bool,
    checkpoints: Arc<Mutex<BTreeSet<String>>>,
    fail_checkpoint: bool,
}

impl Coordinator for Recorder {
    fn persist(&mut self, id: &str, status: &Status) -> Result<(), String> {
        if self.fail_result && *status == Status::Done {
            return Err("disk unavailable on completion".to_string());
        }
        if self.fail_persist.as_deref() == Some(id) && *status == Status::Running {
            return Err("disk unavailable".to_string());
        }
        self.states
            .lock()
            .unwrap()
            .insert(id.to_string(), status.clone());
        Ok(())
    }
    fn confirm(&mut self, job: &Job) -> Result<(), String> {
        self.confirmations.push(job.id.clone());
        Ok(())
    }
    fn checkpoint(&mut self, id: &str, _data: &str) -> Result<(), String> {
        if self.fail_checkpoint {
            return Err("checkpoint failed".to_string());
        }
        self.checkpoints.lock().unwrap().insert(id.to_string());
        Ok(())
    }
    fn progress(&mut self, id: &str, message: &str) {
        self.progress.push(format!("{id}: {message}"));
    }
}

fn job(id: &str, repo: &str, after: &[&str]) -> Job {
    Job {
        id: id.to_string(),
        repo: repo.to_string(),
        base: "main".to_string(),
        after: after.iter().map(|s| s.to_string()).collect(),
        publisher: false,
        barrier: false,
    }
}

#[test]
fn bounds_concurrency_and_serializes_all_bases_of_a_repository() {
    let jobs = [
        job("a", "acme/one", &[]),
        job("b", "acme/one", &[]),
        job("c", "acme/two", &[]),
        job("d", "acme/three", &[]),
    ];
    let current = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let repos = Mutex::new(BTreeSet::new());
    let mut recorder = Recorder::default();
    let persisted = recorder.states.clone();
    let report = run(
        &jobs,
        BTreeMap::new(),
        2,
        FailurePolicy::StopOnPublisher,
        &mut recorder,
        |job, resumed, progress| {
            assert!(!resumed);
            assert_eq!(persisted.lock().unwrap()[&job.id], Status::Running);
            assert!(repos.lock().unwrap().insert(job.repo.clone()));
            let active = current.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(active, Ordering::SeqCst);
            progress.send("waiting for deploy").unwrap();
            std::thread::sleep(Duration::from_millis(20));
            repos.lock().unwrap().remove(&job.repo);
            current.fetch_sub(1, Ordering::SeqCst);
            Ok(Completion::Done)
        },
    )
    .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert!(report.states.values().all(|state| *state == Status::Done));
    assert_eq!(recorder.progress.len(), 4);
}

#[test]
fn dependencies_cross_lanes_and_barriers_drain_all_workers() {
    let mut gate = job("gate", "acme/gate", &["a"]);
    gate.barrier = true;
    let jobs = [
        job("consumer", "acme/consumer", &["b"]),
        job("a", "acme/a", &[]),
        job("b", "acme/b", &["a"]),
        gate,
        job("later", "acme/later", &[]),
    ];
    let done = Mutex::new(BTreeSet::new());
    let running = AtomicUsize::new(0);
    let mut recorder = Recorder::default();
    let report = run(
        &jobs,
        BTreeMap::new(),
        3,
        FailurePolicy::StopOnPublisher,
        &mut recorder,
        |job, _, _| {
            for dependency in &job.after {
                assert!(done.lock().unwrap().contains(dependency));
            }
            let active = running.fetch_add(1, Ordering::SeqCst) + 1;
            if job.barrier {
                assert_eq!(active, 1);
            }
            if job.id == "later" {
                assert!(done.lock().unwrap().contains("gate"));
            }
            std::thread::sleep(Duration::from_millis(5));
            done.lock().unwrap().insert(job.id.clone());
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(Completion::Done)
        },
    )
    .unwrap();
    assert_eq!(recorder.confirmations, ["gate"]);
    assert!(!report.stopped);
}

#[test]
fn publisher_failure_stops_new_work_but_preserves_started_deploy() {
    let mut publisher = job("publisher", "acme/pkg", &[]);
    publisher.publisher = true;
    let jobs = [
        publisher,
        job("running", "acme/service", &[]),
        job("later", "acme/later", &[]),
    ];
    let started = Barrier::new(2);
    let report = run(
        &jobs,
        BTreeMap::new(),
        2,
        FailurePolicy::StopOnPublisher,
        &mut Recorder::default(),
        |job, _, _| {
            assert_ne!(job.id, "later");
            started.wait();
            if job.publisher {
                Err(Failure {
                    message: "publish failed".to_string(),
                    fatal: false,
                })
            } else {
                std::thread::sleep(Duration::from_millis(25));
                Ok(Completion::Done)
            }
        },
    )
    .unwrap();
    assert!(report.stopped);
    assert_eq!(report.states["running"], Status::Done);
    assert_eq!(report.states["later"], Status::Pending);
}

#[test]
fn skip_policy_blocks_descendants_and_continues_independents() {
    let mut publisher = job("a", "acme/pkg", &[]);
    publisher.publisher = true;
    let jobs = [
        publisher,
        job("b", "acme/b", &["a"]),
        job("c", "acme/c", &["b"]),
        job("d", "acme/d", &[]),
    ];
    let report = run(
        &jobs,
        BTreeMap::new(),
        1,
        FailurePolicy::SkipDescendants,
        &mut Recorder::default(),
        |job, _, _| {
            if job.id == "a" {
                Err(Failure {
                    message: "failure".to_string(),
                    fatal: false,
                })
            } else {
                Ok(Completion::Done)
            }
        },
    )
    .unwrap();
    assert!(matches!(report.states["b"], Status::Blocked(_)));
    assert!(matches!(report.states["c"], Status::Blocked(_)));
    assert_eq!(report.states["d"], Status::Done);
}

#[test]
fn leaf_failure_continues_and_fatal_readiness_stops() {
    let jobs = [job("a", "acme/a", &[]), job("b", "acme/b", &[])];
    for fatal in [false, true] {
        let report = run(
            &jobs,
            BTreeMap::new(),
            1,
            FailurePolicy::StopOnPublisher,
            &mut Recorder::default(),
            |job, _, _| {
                if job.id == "a" {
                    Err(Failure {
                        message: "failure".to_string(),
                        fatal,
                    })
                } else {
                    Ok(Completion::Done)
                }
            },
        )
        .unwrap();
        assert_eq!(report.stopped, fatal);
        assert_eq!(
            report.states["b"],
            if fatal { Status::Pending } else { Status::Done }
        );
    }
}

#[test]
fn resumes_running_without_replaying_done_and_skipped_is_nonblocking() {
    let jobs = [
        job("a", "acme/a", &[]),
        job("b", "acme/b", &["a"]),
        job("c", "acme/c", &["b"]),
    ];
    let states = BTreeMap::from([
        ("a".to_string(), Status::Done),
        ("b".to_string(), Status::Running),
    ]);
    let report = run(
        &jobs,
        states,
        2,
        FailurePolicy::StopOnPublisher,
        &mut Recorder::default(),
        |job, resumed, _| {
            assert_ne!(job.id, "a");
            assert_eq!(resumed, job.id == "b");
            Ok(if resumed {
                Completion::NormalSkipped
            } else {
                Completion::Done
            })
        },
    )
    .unwrap();
    assert_eq!(report.states["b"], Status::NormalSkipped);
    assert_eq!(report.states["c"], Status::Done);
}

#[test]
fn persistence_failure_prevents_effects() {
    let mut recorder = Recorder {
        fail_persist: Some("a".to_string()),
        ..Recorder::default()
    };
    let report = run(
        &[job("a", "acme/a", &[])],
        BTreeMap::new(),
        1,
        FailurePolicy::StopOnPublisher,
        &mut recorder,
        |_, _, _| {
            panic!("must persist before effect");
        },
    )
    .unwrap();
    assert!(report.stopped);
    assert_eq!(report.coordinator_errors.len(), 1);
    assert_eq!(report.states["a"], Status::Pending);
}

#[test]
fn rejects_unknown_dependencies_and_cycles() {
    assert!(
        run(
            &[job("a", "acme/a", &["missing"])],
            BTreeMap::new(),
            1,
            FailurePolicy::StopOnPublisher,
            &mut Recorder::default(),
            |_, _, _| Ok(Completion::Done)
        )
        .is_err()
    );
    assert!(
        run(
            &[job("a", "acme/a", &["b"]), job("b", "acme/b", &["a"])],
            BTreeMap::new(),
            1,
            FailurePolicy::StopOnPublisher,
            &mut Recorder::default(),
            |_, _, _| Ok(Completion::Done)
        )
        .is_err()
    );
}

#[test]
fn persisted_running_is_reconciled_even_after_fatal_failure() {
    let jobs = [
        job("failed", "acme/one", &[]),
        job("new", "acme/new", &[]),
        job("running", "acme/two", &["failed"]),
    ];
    let states = BTreeMap::from([
        (
            "failed".to_string(),
            Status::Failed {
                message: "readiness".to_string(),
                fatal: true,
            },
        ),
        ("running".to_string(), Status::Running),
    ]);
    let report = run(
        &jobs,
        states,
        2,
        FailurePolicy::StopOnPublisher,
        &mut Recorder::default(),
        |job, resumed, _| {
            assert_eq!(job.id, "running");
            assert!(resumed);
            Ok(Completion::Done)
        },
    )
    .unwrap();
    assert!(report.stopped);
    assert_eq!(report.states["running"], Status::Done);
    assert_eq!(report.states["new"], Status::Pending);
}

#[test]
fn failed_result_persistence_remains_running_for_next_invocation() {
    let mut recorder = Recorder {
        fail_result: true,
        ..Recorder::default()
    };
    let calls = AtomicUsize::new(0);
    let report = run(
        &[job("a", "acme/a", &[])],
        BTreeMap::new(),
        1,
        FailurePolicy::StopOnPublisher,
        &mut recorder,
        |_, _, _| {
            assert_eq!(calls.fetch_add(1, Ordering::SeqCst), 0);
            Ok(Completion::Done)
        },
    )
    .unwrap();
    assert!(report.stopped);
    assert_eq!(report.states["a"], Status::Running);
    assert_eq!(report.coordinator_errors.len(), 1);
}

#[test]
fn checkpoint_ack_precedes_effect_and_failure_prevents_effect() {
    for fail_checkpoint in [false, true] {
        let mut recorder = Recorder {
            fail_checkpoint,
            ..Recorder::default()
        };
        let persisted = recorder.checkpoints.clone();
        let effects = AtomicUsize::new(0);
        let report = run(
            &[job("a", "acme/a", &[])],
            BTreeMap::new(),
            1,
            FailurePolicy::StopOnPublisher,
            &mut recorder,
            |job, _, progress| {
                progress.checkpoint("pending").map_err(|message| Failure {
                    message,
                    fatal: true,
                })?;
                assert!(persisted.lock().unwrap().contains(&job.id));
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(Completion::Done)
            },
        )
        .unwrap();
        assert_eq!(
            effects.load(Ordering::SeqCst),
            usize::from(!fail_checkpoint)
        );
        assert_eq!(report.stopped, fail_checkpoint);
    }
}
