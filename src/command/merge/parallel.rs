use super::*;
use crate::merge::StoreOperation;
use crate::merge_scheduler::{self as scheduler, Completion, Failure, Job};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum Checkpoint {
    State(StoreOperation),
    Finished(StepReport),
}

struct Coordinator<'a, 'h> {
    env: &'a Env<'a>,
    store: &'a mut Store,
    hooks: &'a mut Hooks<'h>,
    steps: &'a mut [StepReport],
    bundles: &'a BTreeMap<String, Bundled>,
    failures: failure::DeployFailures,
    states: BTreeMap<String, scheduler::Status>,
}

fn step_index(job_id: &str) -> Result<usize, String> {
    job_id
        .strip_prefix("step:")
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| "invalid scheduler step".to_string())
}

impl scheduler::Coordinator for Coordinator<'_, '_> {
    fn persist(&mut self, id: &str, status: &scheduler::Status) -> Result<(), String> {
        let index = step_index(id)?;
        let blocked_by = if let scheduler::Status::Blocked(cause) = status {
            Some(
                step_index(cause)
                    .ok()
                    .and_then(|i| self.steps.get(i))
                    .map(|step| step.target.clone())
                    .unwrap_or_else(|| cause.clone()),
            )
        } else {
            None
        };
        let step = &mut self.steps[index];
        match status {
            scheduler::Status::Running => {
                if !self
                    .store
                    .state
                    .active_jobs
                    .iter()
                    .any(|active| active == id)
                {
                    self.store.state.active_jobs.push(id.to_string());
                }
            }
            scheduler::Status::Blocked(cause) => {
                step.status = Status::Skipped;
                step.blocked_by = blocked_by;
                step.detail = format!("blocked by {}", step.blocked_by.as_deref().unwrap_or(cause));
            }
            scheduler::Status::Done
            | scheduler::Status::NormalSkipped
            | scheduler::Status::Failed { .. } => {
                if let scheduler::Status::Failed { message, .. } = status
                    && step.status == Status::Planned
                {
                    fail(step, message);
                }
                self.store.state.active_jobs.retain(|active| active != id);
            }
            scheduler::Status::Pending => {}
        }
        self.store.save().map_err(|error| error.to_string())?;
        self.states.insert(id.to_string(), status.clone());
        if !matches!(
            status,
            scheduler::Status::Running | scheduler::Status::Pending
        ) {
            (self.hooks.observe)(&self.steps[index]);
        }
        Ok(())
    }

    fn prepare(&mut self, job: &Job) -> Result<Option<scheduler::Status>, String> {
        let index = step_index(&job.id)?;
        Ok(self
            .failures
            .blocker(
                index,
                self.steps,
                self.env.plan,
                self.bundles,
                &self.store.state,
            )
            .map(|blocked| scheduler::Status::Blocked(blocked.id)))
    }

    fn confirm(&mut self, job: &Job) -> Result<(), String> {
        let step = &mut self.steps[step_index(&job.id)?];
        if matches!(step.kind, Kind::Gate | Kind::Wait) {
            gate_step(self.env, self.hooks, step);
            if step.status == Status::Stopped {
                return Err(step.detail.clone());
            }
        }
        Ok(())
    }

    fn progress(&mut self, id: &str, message: &str) {
        if let Ok(index) = step_index(id) {
            let total = self.steps.len();
            let completed = self
                .states
                .values()
                .filter(|status| {
                    !matches!(
                        status,
                        scheduler::Status::Pending | scheduler::Status::Running
                    )
                })
                .count();
            let active = self
                .states
                .values()
                .filter(|status| **status == scheduler::Status::Running)
                .count();
            (self.hooks.progress)(&format!(
                "{completed}/{total} completed, {active} active, {} remaining [{}]: {message}",
                total - completed,
                self.steps[index].target
            ));
        }
    }

    fn checkpoint(&mut self, id: &str, data: &str) -> Result<(), String> {
        let index = step_index(id)?;
        let checkpoint: Checkpoint =
            serde_json::from_str(data).map_err(|_| "invalid worker checkpoint".to_string())?;
        let target = &self.steps[index].target;
        match checkpoint {
            Checkpoint::State(operation) => {
                let valid = match &operation {
                    StoreOperation::Add(pending) => pending.id == *target,
                    StoreOperation::SetSha { id, .. } | StoreOperation::Remove(id) => id == target,
                    StoreOperation::AddDeployPr { id, .. } => id == target,
                    StoreOperation::RemoveDeployPr { slug, branch } => {
                        open_target(slug, branch) == *target
                    }
                };
                if !valid {
                    return Err("checkpoint does not belong to this step".to_string());
                }
                self.store
                    .apply(operation)
                    .map_err(|error| error.to_string())
            }
            Checkpoint::Finished(step) => {
                if step.target != *target || step.kind != self.steps[index].kind {
                    return Err("worker result does not belong to this step".to_string());
                }
                if step.status == Status::Failed
                    && step.merge_sha.is_some()
                    && let Some(pending) = self
                        .store
                        .state
                        .pending
                        .iter()
                        .find(|pending| pending.id == step.target)
                {
                    self.failures.record(pending, self.env.plan);
                }
                self.steps[index] = step;
                Ok(())
            }
        }
    }
}

fn jobs(
    env: &Env,
    steps: &[StepReport],
    bundles: &BTreeMap<String, Bundled>,
    state: &State,
) -> (Vec<Job>, BTreeMap<String, scheduler::Status>) {
    let mut targets = BTreeMap::new();
    for (index, step) in steps.iter().enumerate() {
        if matches!(step.kind, Kind::Merge | Kind::Resume) {
            targets.insert(step.target.clone(), format!("step:{index}"));
        }
    }
    let mut external = BTreeMap::new();
    let mut previous = BTreeMap::new();
    let mut jobs = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let item = env.plan.items.iter().find(|item| item.id == step.target);
        let pending = state
            .pending
            .iter()
            .find(|pending| pending.id == step.target);
        let (repo, base) = if let Some(item) = item {
            (format!("{}/{}", item.org, item.repo), item.base.clone())
        } else if let Some(pending) = pending {
            (pending.repo.clone(), pending.base.clone())
        } else if let Some(bundle) = bundles.get(&step.target) {
            (bundle.slug.clone(), bundle.branch.clone())
        } else {
            (format!("barrier:{index}"), String::new())
        };
        let mut after = Vec::new();
        if step.kind == Kind::Merge
            && let Some(item) = item
        {
            for dependency in &item.after {
                if let Some(target) = targets.get(&dependency.id) {
                    after.push(target.clone());
                } else {
                    after.push(dependency.id.clone());
                }
            }
        }
        if matches!(step.kind, Kind::Gate | Kind::Wait)
            && let Some(next) = steps[index + 1..]
                .iter()
                .find(|step| step.kind == Kind::Merge)
            && let Some(item) = env.plan.items.iter().find(|item| item.id == next.target)
        {
            for dependency in &item.after {
                if let Some(target) = targets.get(&dependency.id) {
                    after.push(target.clone());
                }
            }
        }
        let id = format!("step:{index}");
        if matches!(step.kind, Kind::Merge | Kind::Resume | Kind::OpenPr)
            && let Some(prior) = previous.insert((repo.to_lowercase(), base.clone()), id.clone())
        {
            after.push(prior);
        }
        let publisher = item
            .map(|item| failure::continuation(env.plan, item).has_consumers)
            .or_else(|| {
                pending.and_then(|pending| {
                    pending
                        .continuation
                        .as_ref()
                        .map(|saved| saved.has_consumers)
                })
            })
            .unwrap_or(false);
        if step.kind == Kind::Resume {
            external.insert(id.clone(), scheduler::Status::Running);
        }
        jobs.push(Job {
            id,
            repo: repo.to_lowercase(),
            base,
            after,
            publisher,
            barrier: matches!(
                step.kind,
                Kind::Resume | Kind::Gate | Kind::Wait | Kind::OpenPr
            ),
        });
    }
    (jobs, external)
}

pub(super) fn execute(
    env: &Env,
    client: &Client,
    store: &mut Store,
    hooks: &mut Hooks,
    steps: &mut [StepReport],
    bundles: &BTreeMap<String, Bundled>,
) -> Result<()> {
    store.state.active_jobs.clear();
    store.save()?;
    let initial_steps = steps.to_vec();
    let initial_state = store.state.clone();
    let (jobs, states) = jobs(env, steps, bundles, &initial_state);
    let policy = match env.settings.dependency_failure {
        DependencyFailure::Stop => scheduler::FailurePolicy::StopOnPublisher,
        DependencyFailure::Skip => scheduler::FailurePolicy::SkipDescendants,
    };
    let mut coordinator = Coordinator {
        env,
        store,
        hooks,
        steps,
        bundles,
        failures: failure::DeployFailures::default(),
        states: BTreeMap::new(),
    };
    let plan = env.plan;
    let cfg = env.cfg;
    let settings = env.settings;
    let method = env.method;
    let result = scheduler::run(
        &jobs,
        states,
        cfg.max_parallel,
        policy,
        &mut coordinator,
        |job, _, progress| {
            let index = step_index(&job.id).map_err(|message| Failure {
                message,
                fatal: true,
            })?;
            let mut step = initial_steps[index].clone();
            if matches!(step.kind, Kind::Gate | Kind::Wait) {
                return Ok(Completion::Done);
            }
            let mut client = client.fork();
            let sender = progress.clone();
            let mut local = Store::remote(initial_state.clone(), move |operation| {
                let data = serde_json::to_string(&Checkpoint::State(operation))?;
                sender.checkpoint(data).map_err(anyhow::Error::msg)
            });
            let env = Env {
                plan,
                cfg,
                settings,
                method,
                readings: RefCell::new(BTreeMap::new()),
            };
            let mut confirm = |_: &str| false;
            let mut gate = |_: &str| false;
            let mut observe = |_: &StepReport| {};
            let mut sleep = std::thread::sleep;
            let mut emit = |message: &str| {
                let _ = progress.send(message);
            };
            let mut hooks = Hooks {
                confirm: &mut confirm,
                gate: &mut gate,
                observe: &mut observe,
                sleep: &mut sleep,
                progress: &mut emit,
            };
            (hooks.progress)("starting");
            match step.kind {
                Kind::Merge => match plan.items.iter().find(|item| item.id == step.target) {
                    Some(item) => {
                        merge_one(&env, item, &mut client, &mut local, &mut hooks, &mut step)
                    }
                    None => fail(&mut step, "it is not in the plan"),
                },
                Kind::Resume => match initial_state
                    .pending
                    .iter()
                    .find(|pending| pending.id == step.target)
                {
                    Some(pending) => resume_one(
                        &env,
                        pending,
                        &mut client,
                        &mut local,
                        &mut hooks,
                        &mut step,
                    ),
                    None => fail(&mut step, "the saved state no longer lists it"),
                },
                Kind::OpenPr => match bundles.get(&step.target) {
                    Some(bundle) => {
                        if open_deploy_pr(&env, plan, bundle, &mut client, &mut step)
                            && let Err(error) = local.remove_deploy_pr(&bundle.slug, &bundle.branch)
                        {
                            stop(
                                &mut step,
                                format!("cannot update the merge state: {error:#}"),
                            );
                        }
                    }
                    None => fail(&mut step, "the bundle is not in this run"),
                },
                _ => {}
            }
            let failed = matches!(step.status, Status::Failed | Status::Stopped);
            let fatal = step.status == Status::Stopped
                || step.merge_sha.is_none()
                || !local
                    .state
                    .pending
                    .iter()
                    .any(|pending| pending.id == step.target && pending.continuation.is_some());
            let outcome = if failed {
                Err(Failure {
                    message: step.detail.clone(),
                    fatal,
                })
            } else if step.status == Status::Skipped {
                Ok(Completion::NormalSkipped)
            } else {
                Ok(Completion::Done)
            };
            let data =
                serde_json::to_string(&Checkpoint::Finished(step)).map_err(|error| Failure {
                    message: error.to_string(),
                    fatal: true,
                })?;
            progress.checkpoint(data).map_err(|message| Failure {
                message,
                fatal: true,
            })?;
            outcome
        },
    )
    .map_err(anyhow::Error::msg)?;
    let stopped_by = result.states.iter().find_map(|(id, status)| {
        matches!(status, scheduler::Status::Failed { .. })
            .then(|| {
                step_index(id)
                    .ok()
                    .and_then(|index| coordinator.steps.get(index))
                    .map(|step| step.target.clone())
            })
            .flatten()
    });
    for (id, status) in &result.states {
        if matches!(status, scheduler::Status::Pending)
            && let Ok(index) = step_index(id)
        {
            let step = &mut coordinator.steps[index];
            step.status = Status::Skipped;
            step.detail = STOPPED.to_string();
            step.blocked_by = stopped_by.clone();
            (coordinator.hooks.observe)(step);
        }
    }
    if !result.coordinator_errors.is_empty() {
        bail!(
            "parallel coordinator failed: {}",
            result.coordinator_errors.join("; ")
        );
    }
    Ok(())
}
