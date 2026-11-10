use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, SyncSender};
use std::thread;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub repo: String,
    pub base: String,
    pub after: Vec<String>,
    pub publisher: bool,
    pub barrier: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Done,
    NormalSkipped,
    Failed { message: String, fatal: bool },
    Blocked(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Done,
    NormalSkipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub fatal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailurePolicy {
    StopOnPublisher,
    SkipDescendants,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Transition {
        id: String,
        status: Status,
        fatal: bool,
    },
    Progress {
        id: String,
        message: String,
    },
}

pub trait Coordinator {
    fn persist(&mut self, id: &str, status: &Status) -> Result<(), String>;
    fn confirm(&mut self, job: &Job) -> Result<(), String>;
    fn progress(&mut self, id: &str, message: &str);
}

#[derive(Clone)]
pub struct Progress {
    id: String,
    sender: SyncSender<Event>,
}

impl Progress {
    pub fn send(&self, message: impl Into<String>) -> Result<(), String> {
        self.sender
            .send(Event::Progress {
                id: self.id.clone(),
                message: message.into(),
            })
            .map_err(|_| "scheduler stopped receiving progress".to_string())
    }
}

#[derive(Debug)]
pub struct Report {
    pub states: BTreeMap<String, Status>,
    pub coordinator_errors: Vec<String>,
    pub stopped: bool,
}

fn transition(
    coordinator: &mut impl Coordinator,
    report: &mut Report,
    id: &str,
    status: Status,
) -> bool {
    match coordinator.persist(id, &status) {
        Ok(()) => {
            report.states.insert(id.to_string(), status);
            true
        }
        Err(error) => {
            report.coordinator_errors.push(format!("{id}: {error}"));
            report.stopped = true;
            false
        }
    }
}

pub fn run<W>(
    jobs: &[Job],
    states: BTreeMap<String, Status>,
    max_parallel: usize,
    policy: FailurePolicy,
    coordinator: &mut impl Coordinator,
    worker: W,
) -> Result<Report, String>
where
    W: Fn(&Job, bool, Progress) -> Result<Completion, Failure> + Sync,
{
    if max_parallel == 0 {
        return Err("max_parallel must be positive".to_string());
    }
    let mut ids = BTreeSet::new();
    for job in jobs {
        if !ids.insert(job.id.clone()) {
            return Err(format!("duplicate job {}", job.id));
        }
    }
    for job in jobs {
        for dependency in &job.after {
            if !ids.contains(dependency)
                && !matches!(
                    states.get(dependency),
                    Some(Status::Done | Status::NormalSkipped)
                )
            {
                return Err(format!("{} has unresolved dependency {dependency}", job.id));
            }
        }
    }
    let mut report = Report {
        states,
        coordinator_errors: Vec::new(),
        stopped: false,
    };
    for job in jobs {
        report
            .states
            .entry(job.id.clone())
            .or_insert(Status::Pending);
    }
    if jobs.iter().any(|job| {
        matches!(report.states[&job.id], Status::Failed { fatal: true, .. })
            || (policy == FailurePolicy::StopOnPublisher
                && job.publisher
                && matches!(report.states[&job.id], Status::Failed { .. }))
    }) {
        report.stopped = true;
    }
    let (sender, receiver) = mpsc::sync_channel(128);
    thread::scope(|scope| {
        let mut active = BTreeSet::new();
        let mut attempted = BTreeSet::new();
        let mut active_repos = BTreeSet::new();
        let mut barrier_active = false;
        loop {
            let mut changed = false;
            let recovering = jobs.iter().any(|job| {
                report.states[&job.id] == Status::Running && !attempted.contains(&job.id)
            });
            if !report.stopped || recovering {
                for (index, job) in jobs.iter().enumerate() {
                    if active.len() >= max_parallel || barrier_active {
                        break;
                    }
                    if attempted.contains(&job.id)
                        || !matches!(report.states[&job.id], Status::Pending | Status::Running)
                    {
                        continue;
                    }
                    let resumed = report.states[&job.id] == Status::Running;
                    if !resumed && (report.stopped || recovering) {
                        continue;
                    }
                    if !resumed
                        && let Some(dependency) = job.after.iter().find(|&id| {
                            matches!(
                                report.states.get(id),
                                Some(Status::Failed { .. } | Status::Blocked(_))
                            )
                        })
                    {
                        changed |= transition(
                            coordinator,
                            &mut report,
                            &job.id,
                            Status::Blocked(dependency.clone()),
                        );
                        if report.stopped {
                            break;
                        }
                        continue;
                    }
                    if !resumed
                        && job.barrier
                        && (!active.is_empty()
                            || jobs[..index].iter().any(|prior| {
                                matches!(
                                    report.states[&prior.id],
                                    Status::Pending | Status::Running
                                )
                            }))
                    {
                        break;
                    }
                    if !resumed
                        && !job.after.iter().all(|id| {
                            matches!(
                                report.states.get(id),
                                Some(Status::Done | Status::NormalSkipped)
                            )
                        })
                    {
                        if job.barrier {
                            break;
                        }
                        continue;
                    }
                    if active_repos.contains(&job.repo) {
                        continue;
                    }
                    if !resumed
                        && job.barrier
                        && let Err(error) = coordinator.confirm(job)
                    {
                        transition(
                            coordinator,
                            &mut report,
                            &job.id,
                            Status::Failed {
                                message: error,
                                fatal: true,
                            },
                        );
                        report.stopped = true;
                        break;
                    }
                    if !transition(coordinator, &mut report, &job.id, Status::Running) {
                        break;
                    }
                    active.insert(job.id.clone());
                    attempted.insert(job.id.clone());
                    active_repos.insert(job.repo.clone());
                    barrier_active = job.barrier;
                    changed = true;
                    let sender = sender.clone();
                    let worker = &worker;
                    scope.spawn(move || {
                        let progress = Progress {
                            id: job.id.clone(),
                            sender: sender.clone(),
                        };
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            worker(job, resumed, progress)
                        }));
                        let (status, fatal) = match result {
                            Ok(Ok(Completion::Done)) => (Status::Done, false),
                            Ok(Ok(Completion::NormalSkipped)) => (Status::NormalSkipped, false),
                            Ok(Err(error)) => (
                                Status::Failed {
                                    message: error.message,
                                    fatal: error.fatal,
                                },
                                error.fatal,
                            ),
                            Err(_) => (
                                Status::Failed {
                                    message: "worker panicked".to_string(),
                                    fatal: true,
                                },
                                true,
                            ),
                        };
                        let _ = sender.send(Event::Transition {
                            id: job.id.clone(),
                            status,
                            fatal,
                        });
                    });
                }
            }
            if active.is_empty() {
                if changed && !report.stopped {
                    continue;
                }
                break;
            }
            match receiver.recv() {
                Ok(Event::Progress { id, message }) => coordinator.progress(&id, &message),
                Ok(Event::Transition { id, status, fatal }) => {
                    let job = jobs.iter().find(|job| job.id == id).unwrap();
                    active.remove(&id);
                    active_repos.remove(&job.repo);
                    barrier_active = false;
                    if fatal
                        || (matches!(status, Status::Failed { .. })
                            && job.publisher
                            && policy == FailurePolicy::StopOnPublisher)
                    {
                        report.stopped = true;
                    }
                    transition(coordinator, &mut report, &id, status);
                }
                Err(_) => break,
            }
        }
    });
    if !report.stopped
        && jobs
            .iter()
            .any(|job| matches!(report.states[&job.id], Status::Pending | Status::Running))
    {
        return Err("dependency graph cannot make progress".to_string());
    }
    Ok(report)
}
