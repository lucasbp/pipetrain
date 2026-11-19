use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Env, Hooks};
use crate::config::Config;
use crate::merge::{Kind, State, StepReport};
use crate::plan::Plan;
use crate::workflows::PushRuns;

fn key(slug: &str, base: &str) -> String {
    format!("{}@{base}", slug.to_lowercase())
}

#[derive(Clone, Default)]
pub(super) struct Landings(Arc<Mutex<BTreeMap<String, Instant>>>);

impl Landings {
    pub(super) fn record(&self, slug: &str, base: &str, at: Instant) {
        if let Ok(mut landed) = self.0.lock() {
            landed.insert(key(slug, base), at);
        }
    }

    fn last(&self, slug: &str, base: &str) -> Option<Instant> {
        self.0.lock().ok()?.get(&key(slug, base)).copied()
    }
}

pub(super) fn wait(env: &Env, slug: &str, base: &str, id: &str, hooks: &mut Hooks) {
    let window = env.cfg.merge.same_target_settle(slug, base);
    if window.is_zero() {
        return;
    }
    let Some(landed) = env.landings.last(slug, base) else {
        return;
    };
    let cap = env
        .settings
        .timeout
        .unwrap_or_else(|| env.cfg.merge.timeout_for(slug));
    let mut waited = Duration::ZERO;
    let mut announced = false;
    loop {
        let elapsed = (hooks.now)().saturating_duration_since(landed);
        let pause = window
            .saturating_sub(elapsed)
            .min(cap.saturating_sub(waited));
        if pause.is_zero() {
            return;
        }
        if !announced {
            announced = true;
            (hooks.progress)(&format!(
                "settling {}s before merging {id}: the previous deploy on {slug}@{base} may still be releasing",
                pause.as_secs().max(1)
            ));
        }
        (hooks.sleep)(pause);
        waited += pause;
    }
}

fn reference(step: &StepReport, plan: &Plan, state: &State) -> Option<(String, String)> {
    match step.kind {
        Kind::Merge => plan
            .items
            .iter()
            .find(|item| item.id == step.target)
            .map(|item| (format!("{}/{}", item.org, item.repo), item.base.clone())),
        Kind::Resume => state
            .pending
            .iter()
            .find(|pending| pending.id == step.target)
            .map(|pending| (pending.repo.clone(), pending.base.clone())),
        _ => None,
    }
}

pub(super) fn annotate(steps: &mut [StepReport], plan: &Plan, cfg: &Config, state: &State) {
    let mut seen: Vec<String> = Vec::new();
    for step in steps.iter_mut() {
        let Some((slug, base)) = reference(step, plan, state) else {
            continue;
        };
        let target = key(&slug, &base);
        let window = cfg.merge.same_target_settle(&slug, &base).as_secs();
        if window > 0 && seen.contains(&target) && step.kind == Kind::Merge {
            step.settle_seconds = Some(window);
        }
        let dispensed = !cfg.merge.waits_for_deploy(&slug)
            || matches!(step.deploy_prediction, Some(PushRuns::Dispensed { .. }));
        if !dispensed {
            seen.push(target);
        }
    }
}
