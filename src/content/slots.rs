use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use super::{Check, MAX_LOOKUPS, Probe, Reading, build_query, parse};
use crate::github::Client;

type Key = (String, String);
type Oids = BTreeMap<String, Option<String>>;
pub type Maps = BTreeMap<Key, Oids>;

#[derive(Debug, Clone)]
pub struct Slot {
    index: usize,
    key: Key,
    path: String,
}

pub fn fixed(checks: &[Check]) -> Vec<Slot> {
    let mut out = Vec::new();
    let mut cursor = 0;
    for check in checks {
        for (i, path) in check.paths.iter().enumerate() {
            out.push(Slot {
                index: cursor + 2 + 2 * i,
                key: (check.slug.clone(), check.sha.clone()),
                path: path.clone(),
            });
        }
        cursor += 1 + 2 * check.paths.len();
    }
    out
}

pub fn load(client: &mut Client, slots: &[Slot]) -> Maps {
    let mut maps = Maps::new();
    for slot in slots {
        if !maps.contains_key(&slot.key) {
            let oids = client.oids(&slot.key.0, &slot.key.1);
            maps.insert(slot.key.clone(), oids);
        }
    }
    maps
}

pub fn prefill(total: usize, slots: &[Slot], maps: &Maps) -> Vec<Reading> {
    let mut readings = vec![None; total];
    for slot in slots {
        if let Some(oid) = maps.get(&slot.key).and_then(|oids| oids.get(&slot.path)) {
            readings[slot.index] = Some(oid.clone());
        }
    }
    readings
}

pub fn unknown(slots: &[Slot], readings: &[Reading]) -> Vec<Slot> {
    slots
        .iter()
        .filter(|slot| readings[slot.index].is_none())
        .cloned()
        .collect()
}

pub fn ask(client: &mut Client, lookups: &[Probe], readings: &mut [Reading]) -> Result<()> {
    let pending: Vec<usize> = readings
        .iter()
        .enumerate()
        .filter(|(_, reading)| reading.is_none())
        .map(|(index, _)| index)
        .collect();
    for indexes in pending.chunks(MAX_LOOKUPS) {
        let chunk: Vec<Probe> = indexes.iter().map(|&i| lookups[i].clone()).collect();
        let (query, variables) = build_query(&chunk);
        let response =
            client.graphql("compare PR file content with the base", &query, variables)?;
        for (&index, reading) in indexes.iter().zip(parse(&response, chunk.len())) {
            readings[index] = reading;
        }
    }
    Ok(())
}

pub fn learn(maps: &mut Maps, unknown: &[Slot], readings: &[Reading]) -> BTreeSet<Key> {
    let mut changed = BTreeSet::new();
    for slot in unknown {
        if let Some(oid) = &readings[slot.index] {
            maps.entry(slot.key.clone())
                .or_default()
                .insert(slot.path.clone(), oid.clone());
            changed.insert(slot.key.clone());
        }
    }
    changed
}

pub fn save(client: &mut Client, maps: &Maps, changed: &BTreeSet<Key>) {
    for key in changed {
        if let Some(oids) = maps.get(key) {
            client.store_oids(&key.0, &key.1, oids);
        }
    }
}
