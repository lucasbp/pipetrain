use std::collections::{BTreeMap, BTreeSet};

use crate::publishers::Pairs;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub consumer: String,
    pub package: String,
    pub publisher: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub repos: Vec<String>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub chains: Vec<Chain>,
    pub cycles: Vec<(String, String)>,
}

type Adjacency = BTreeMap<String, BTreeSet<String>>;

pub fn analyze(pairs: &Pairs) -> Analysis {
    let (forward, backward) = adjacency(pairs);
    let mut analysis = Analysis::default();
    for members in components(&forward, &backward) {
        match order(&members, &forward, &backward) {
            Ok(repos) => analysis.chains.push(chain(repos, pairs)),
            Err(cycle) => analysis.cycles.push(cycle),
        }
    }
    analysis
}

fn adjacency(pairs: &Pairs) -> (Adjacency, Adjacency) {
    let mut forward = Adjacency::new();
    let mut backward = Adjacency::new();
    for (publisher, consumer) in pairs.keys() {
        forward
            .entry(publisher.clone())
            .or_default()
            .insert(consumer.clone());
        forward.entry(consumer.clone()).or_default();
        backward
            .entry(consumer.clone())
            .or_default()
            .insert(publisher.clone());
        backward.entry(publisher.clone()).or_default();
    }
    (forward, backward)
}

fn components(forward: &Adjacency, backward: &Adjacency) -> Vec<BTreeSet<String>> {
    let mut seen: BTreeSet<&String> = BTreeSet::new();
    let mut found = Vec::new();
    for start in forward.keys() {
        if seen.contains(start) {
            continue;
        }
        let mut members = BTreeSet::new();
        let mut queue = vec![start];
        while let Some(node) = queue.pop() {
            if !seen.insert(node) {
                continue;
            }
            members.insert(node.clone());
            queue.extend(forward[node].iter().chain(backward[node].iter()));
        }
        found.push(members);
    }
    found
}

fn order(
    members: &BTreeSet<String>,
    forward: &Adjacency,
    backward: &Adjacency,
) -> Result<Vec<String>, (String, String)> {
    let mut waiting: BTreeMap<&String, usize> =
        members.iter().map(|m| (m, backward[m].len())).collect();
    let mut ready: BTreeSet<&String> = waiting
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(m, _)| *m)
        .collect();
    let mut sorted: Vec<String> = Vec::new();
    while let Some(next) = ready.pop_first() {
        sorted.push(next.clone());
        for consumer in &forward[next] {
            if let Some(left) = waiting.get_mut(consumer) {
                *left -= 1;
                if *left == 0 {
                    ready.insert(consumer);
                }
            }
        }
    }
    if sorted.len() == members.len() {
        return Ok(sorted);
    }
    Err(find_cycle(members, &sorted, backward))
}

fn find_cycle(
    members: &BTreeSet<String>,
    sorted: &[String],
    backward: &Adjacency,
) -> (String, String) {
    let remaining: BTreeSet<&String> = members.iter().filter(|m| !sorted.contains(*m)).collect();
    let mut path: Vec<&String> = Vec::new();
    let mut node = remaining.first().copied();
    while let Some(current) = node {
        if let Some(at) = path.iter().position(|seen| *seen == current) {
            return cycle_pair(&path[at..]);
        }
        path.push(current);
        node = backward[current].iter().find(|p| remaining.contains(*p));
    }
    (String::new(), String::new())
}

fn cycle_pair(cycle: &[&String]) -> (String, String) {
    let first = (0..cycle.len()).min_by_key(|i| cycle[*i]).unwrap_or(0);
    (
        cycle[first].clone(),
        cycle[(first + 1) % cycle.len()].clone(),
    )
}

fn chain(repos: Vec<String>, pairs: &Pairs) -> Chain {
    let members: BTreeSet<&String> = repos.iter().collect();
    let mut edges: Vec<Edge> = pairs
        .iter()
        .filter(|((publisher, _), _)| members.contains(publisher))
        .map(|((publisher, consumer), package)| Edge {
            consumer: consumer.clone(),
            package: package.clone(),
            publisher: publisher.clone(),
        })
        .collect();
    edges.sort();
    Chain { repos, edges }
}
