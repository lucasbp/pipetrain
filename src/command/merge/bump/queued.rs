use anyhow::{Context, Result, bail};

use super::{GithubGateway, Options, publication_inputs};
use crate::github::Client;
use crate::merge::Store;
use crate::plan::Plan;
use crate::planner::behind::{QueuedBump, stable_in_major};
use crate::publication::{PublicationEvidence, PublicationResult};

fn remember(contracts: &mut Vec<QueuedBump>, current: &QueuedBump) {
    let Some(saved) = contracts.iter_mut().find(|saved| {
        saved.id == current.id && saved.package.eq_ignore_ascii_case(&current.package)
    }) else {
        contracts.push(current.clone());
        return;
    };
    for (existing, found) in [
        (&mut saved.publishers, &current.publishers),
        (&mut saved.owners, &current.owners),
    ] {
        existing.extend(found.iter().cloned());
        existing.sort();
        existing.dedup();
    }
}

pub fn record_selection(store: &mut Store, contracts: &[QueuedBump]) -> Result<()> {
    if !contracts.is_empty() {
        let journal = store.state.bump.get_or_insert_with(Default::default);
        for contract in contracts {
            remember(&mut journal.queued_bumps, contract);
        }
        store.save()?;
    }
    Ok(())
}

pub fn contracts(plan: &Plan, store: &Store, enabled: bool) -> Vec<QueuedBump> {
    let mut contracts = store
        .state
        .bump
        .as_ref()
        .map(|journal| journal.queued_bumps.clone())
        .unwrap_or_default();
    if enabled {
        for current in &plan.queued_bumps {
            remember(&mut contracts, current);
        }
    }
    contracts.retain(|contract| {
        plan.items
            .iter()
            .any(|item| !item.hidden && item.id == contract.id)
            || plan
                .skipped
                .iter()
                .any(|skipped| !skipped.hidden && skipped.id == contract.id)
    });
    contracts
}

pub fn publication(
    contract: &QueuedBump,
    client: &mut Client,
    options: &Options,
    store: &mut Store,
) -> Result<PublicationEvidence> {
    let [publisher_id] = contract.publishers.as_slice() else {
        bail!(
            "{} needs one publisher PR for {}, candidates: {}",
            contract.id,
            contract.package,
            contract.publishers.join(", ")
        )
    };
    let [owner] = contract.owners.as_slice() else {
        bail!(
            "{} has ambiguous publishing repositories: {}",
            contract.package,
            contract.owners.join(", ")
        )
    };
    let journal = store
        .state
        .bump
        .as_mut()
        .context("waiting for the queued publisher to merge and deploy")?;
    if !journal.queued_bumps.iter().any(|saved| saved == contract) {
        bail!(
            "no recorded publication intent for {} from {publisher_id}",
            contract.package
        )
    }
    let publisher = journal
        .publishers
        .get_mut(publisher_id)
        .with_context(|| format!("waiting for publisher {publisher_id} to merge"))?;
    if !owner.eq_ignore_ascii_case(&publisher.repository)
        || !publisher
            .packages
            .iter()
            .any(|package| package.eq_ignore_ascii_case(&contract.package))
    {
        bail!(
            "publisher {publisher_id} does not match the recorded package {}",
            contract.package
        )
    }
    if !publisher.deploy_confirmed {
        bail!("waiting for a confirmed deploy of publisher {publisher_id}")
    }
    let key = contract.package.to_ascii_lowercase();
    let known = publisher.publications.get(&key);
    let results = GithubGateway { client, options }.verified_runs(
        publisher,
        &contract.package,
        known.map(Vec::as_slice),
        true,
    )?;
    let inputs = publication_inputs(&results);
    let mut published = results.into_iter().filter_map(|result| match result {
        PublicationResult::Published { evidence } => Some(evidence),
        PublicationResult::Skipped { .. } => None,
    });
    let Some(evidence) = published.next() else {
        bail!(
            "no new push of {} was proved for {publisher_id}",
            contract.package
        )
    };
    let version = evidence.version();
    if let Some(second) = published.find(|other| other.version() != version) {
        bail!(
            "publisher {publisher_id} has multiple proved targets for {}: {version}, {}",
            contract.package,
            second.version()
        )
    }
    if stable_in_major(&contract.from, &[version.to_owned()]).is_none() {
        bail!(
            "proved {} {version} cannot bump {} to a newer stable version in the same major",
            contract.package,
            contract.from
        )
    }
    publisher.publications.insert(key, inputs);
    store.save()?;
    Ok(evidence)
}
