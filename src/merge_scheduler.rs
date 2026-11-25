use std::collections::{BTreeMap, BTreeSet, VecDeque};
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

#[derive(Clone, Debug)]
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
    Checkpoint {
        id: String,
        data: String,
        reply: SyncSender<Result<(), String>>,
    },
    Runner {
        id: String,
        waiting: bool,
        reply: Option<SyncSender<()>>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Capacity {
    pub slots: BTreeMap<String, usize>,
    pub resources: BTreeMap<String, Vec<String>>,
}

impl Capacity {
    fn limit(&self, resource: &str) -> usize {
        self.slots.get(resource).copied().unwrap_or(1).max(1)
    }

    fn needs(&self, id: &str) -> &[String] {
        self.resources.get(id).map_or(&[], Vec::as_slice)
    }
}

pub trait Coordinator {
    fn persist(&mut self, id: &str, status: &Status) -> Result<(), String>;
    fn prepare(&mut self, _job: &Job) -> Result<Option<Status>, String> {
        Ok(None)
    }
    fn confirm(&mut self, job: &Job) -> Result<(), String>;
    fn progress(&mut self, id: &str, message: &str);
    fn isolated(&self, _id: &str) -> bool {
        false
    }
    fn checkpoint(&mut self, _id: &str, _data: &str) -> Result<(), String> {
        Err("coordinator does not support worker checkpoints".to_string())
    }
}

#[derive(Clone)]
pub struct Progress {
    id: String,
    sender: SyncSender<Event>,
}

impl Progress {
    pub fn checkpoint(&self, data: impl Into<String>) -> Result<(), String> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Event::Checkpoint {
                id: self.id.clone(),
                data: data.into(),
                reply,
            })
            .map_err(|_| "scheduler stopped receiving checkpoints".to_string())?;
        receive
            .recv()
            .map_err(|_| "scheduler did not acknowledge checkpoint".to_string())?
    }

    pub fn waiting_for_runner(&self, waiting: bool) -> Result<(), String> {
        if waiting {
            return self
                .sender
                .send(Event::Runner {
                    id: self.id.clone(),
                    waiting: true,
                    reply: None,
                })
                .map_err(|_| "scheduler stopped receiving progress".to_string());
        }
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Event::Runner {
                id: self.id.clone(),
                waiting: false,
                reply: Some(reply),
            })
            .map_err(|_| "scheduler stopped receiving progress".to_string())?;
        receive
            .recv()
            .map_err(|_| "scheduler did not grant a slot".to_string())
    }

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
    run_with(
        jobs,
        states,
        max_parallel,
        &Capacity::default(),
        policy,
        coordinator,
        worker,
    )
}

pub fn run_with<W>(
    jobs: &[Job],
    states: BTreeMap<String, Status>,
    max_parallel: usize,
    capacity: &Capacity,
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
        let mut waiting: BTreeSet<String> = BTreeSet::new();
        let mut regrants: VecDeque<(String, SyncSender<()>)> = VecDeque::new();
        let mut held: BTreeMap<String, usize> = BTreeMap::new();
        loop {
            let mut changed = false;
            while active.len() - waiting.len() < max_parallel
                && let Some((id, reply)) = regrants.pop_front()
            {
                waiting.remove(&id);
                let _ = reply.send(());
            }
            let recovering = jobs.iter().any(|job| {
                report.states[&job.id] == Status::Running && !attempted.contains(&job.id)
            });
            if !report.stopped || recovering {
                for (index, job) in jobs.iter().enumerate() {
                    if active.len() - waiting.len() >= max_parallel || barrier_active {
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
                    if capacity.needs(&job.id).iter().any(|resource| {
                        held.get(resource).copied().unwrap_or(0) >= capacity.limit(resource)
                    }) {
                        continue;
                    }
                    if !resumed {
                        match coordinator.prepare(job) {
                            Ok(None) => {}
                            Ok(Some(status @ Status::Blocked(_))) => {
                                changed |= transition(coordinator, &mut report, &job.id, status);
                                if report.stopped {
                                    break;
                                }
                                continue;
                            }
                            Ok(Some(_)) => {
                                report.coordinator_errors.push(format!(
                                    "{}: prepare must return Blocked or None",
                                    job.id
                                ));
                                report.stopped = true;
                                break;
                            }
                            Err(error) => {
                                report
                                    .coordinator_errors
                                    .push(format!("{}: {error}", job.id));
                                report.stopped = true;
                                break;
                            }
                        }
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
                    for resource in capacity.needs(&job.id) {
                        *held.entry(resource.clone()).or_default() += 1;
                    }
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
                Ok(Event::Checkpoint { id, data, reply }) => {
                    let result = coordinator.checkpoint(&id, &data);
                    if let Err(error) = &result {
                        report.coordinator_errors.push(format!("{id}: {error}"));
                        report.stopped = true;
                    }
                    let _ = reply.send(result);
                }
                Ok(Event::Runner {
                    id, waiting: true, ..
                }) => {
                    waiting.insert(id);
                }
                Ok(Event::Runner {
                    id,
                    waiting: false,
                    reply,
                }) => {
                    if let Some(reply) = reply {
                        regrants.push_back((id, reply));
                    }
                }
                Ok(Event::Transition { id, status, fatal }) => {
                    let job = jobs.iter().find(|job| job.id == id).unwrap();
                    active.remove(&id);
                    waiting.remove(&id);
                    active_repos.remove(&job.repo);
                    for resource in capacity.needs(&id) {
                        if let Some(count) = held.get_mut(resource) {
                            *count = count.saturating_sub(1);
                        }
                    }
                    barrier_active = false;
                    if fatal
                        || (matches!(status, Status::Failed { .. })
                            && job.publisher
                            && policy == FailurePolicy::StopOnPublisher
                            && !coordinator.isolated(&id))
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
