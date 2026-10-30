use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::config::{GateKind, Rules};
use crate::detect::{self, Dep};
use crate::model::{Pr, PrKey, Question};
use crate::publishers::{Publishers, References};

mod dependency;
mod order;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    DependsOn,
    Package,
    Stacked,
    Order,
    Dependency,
    Target,
    Layer,
    Gate,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::DependsOn => "depends-on",
            Rule::Package => "package",
            Rule::Stacked => "stacked",
            Rule::Order => "order",
            Rule::Dependency => "dependency",
            Rule::Target => "target",
            Rule::Layer => "layer",
            Rule::Gate => "gate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepState {
    Merged,
    Closed,
    Open,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dependency {
    pub id: String,
    pub rule: Rule,
    pub reason: String,
    pub implied: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: String,
    pub org: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head: String,
    pub base: String,
    pub sha: String,
    pub layer: Option<String>,
    #[serde(skip)]
    pub layer_rank: Option<usize>,
    pub scope: String,
    pub gate: Option<GateKind>,
    pub gate_reason: Option<String>,
    pub manual_pause: bool,
    pub stage: usize,
    pub lane: Option<usize>,
    pub after: Vec<Dependency>,
}

pub struct PlanInput<'a> {
    pub rules: &'a Rules,
    pub prs: Vec<(Pr, String)>,
    pub external: BTreeMap<String, DepState>,
    pub publishers: Publishers,
    pub references: References,
}

#[derive(Debug)]
pub struct Planned {
    pub items: Vec<Item>,
    pub lanes: Vec<Vec<String>>,
    pub gate: Vec<String>,
    pub questions: Vec<Question>,
    pub notes: Vec<String>,
    pub peak_parallel: usize,
}

struct Edge {
    from: usize,
    rule: Rule,
    reason: String,
}

#[derive(PartialEq, Eq)]
enum Added {
    Yes,
    Exists,
    Cycle,
}

struct Graph {
    preds: Vec<Vec<Edge>>,
    succs: Vec<Vec<usize>>,
}

impl Graph {
    fn new(count: usize) -> Self {
        Self {
            preds: (0..count).map(|_| Vec::new()).collect(),
            succs: vec![Vec::new(); count],
        }
    }

    fn reaches(&self, from: usize, to: usize) -> bool {
        let mut seen = vec![false; self.succs.len()];
        let mut stack = vec![from];
        while let Some(node) = stack.pop() {
            if node == to {
                return true;
            }
            if std::mem::replace(&mut seen[node], true) {
                continue;
            }
            stack.extend(self.succs[node].iter().copied());
        }
        false
    }

    fn add(&mut self, from: usize, to: usize, rule: Rule, reason: String) -> Added {
        if from == to {
            return Added::Cycle;
        }
        if self.preds[to].iter().any(|e| e.from == from) {
            return Added::Exists;
        }
        if self.reaches(to, from) {
            return Added::Cycle;
        }
        self.preds[to].push(Edge { from, rule, reason });
        self.succs[from].push(to);
        Added::Yes
    }

    fn stages(&self) -> Vec<usize> {
        let count = self.preds.len();
        let mut indegree: Vec<usize> = self
            .preds
            .iter()
            .map(|p| p.iter().filter(|e| e.rule != Rule::Gate).count())
            .collect();
        let mut stage = vec![1usize; count];
        let mut queue: VecDeque<usize> = (0..count).filter(|&i| indegree[i] == 0).collect();
        while let Some(node) = queue.pop_front() {
            for &next in &self.succs[node] {
                let gate_edge = self.preds[next]
                    .iter()
                    .any(|e| e.from == node && e.rule == Rule::Gate);
                if gate_edge {
                    continue;
                }
                stage[next] = stage[next].max(stage[node] + 1);
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    queue.push_back(next);
                }
            }
        }
        stage
    }
}

struct Builder<'a> {
    rules: &'a Rules,
    prs: Vec<Pr>,
    ids: Vec<String>,
    lookup: HashMap<String, usize>,
    by_repo: HashMap<String, Vec<usize>>,
    gates: Vec<Option<GateKind>>,
    graph: Graph,
    questions: Vec<Question>,
    notes: Vec<String>,
}

impl<'a> Builder<'a> {
    fn new(rules: &'a Rules, prs: Vec<Pr>) -> Self {
        let gates = prs
            .iter()
            .map(|p| rules.gate_of(p).map(|(kind, _)| kind))
            .collect();
        let ids: Vec<String> = prs.iter().map(|p| p.key.to_string()).collect();
        let lookup = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.to_lowercase(), i))
            .collect();
        let mut by_repo: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, pr) in prs.iter().enumerate() {
            by_repo
                .entry(pr.key.slug().to_lowercase())
                .or_default()
                .push(i);
        }
        let graph = Graph::new(prs.len());
        Self {
            rules,
            prs,
            ids,
            lookup,
            by_repo,
            gates,
            graph,
            questions: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn ask(&mut self, idx: usize, code: &str, message: String, hint: &str) {
        let key = self.prs[idx].key.clone();
        self.questions
            .push(Question::new(Some(&key), code, message, hint));
    }

    fn link(&mut self, from: usize, to: usize, rule: Rule, reason: String, flag: bool) {
        let result = self.graph.add(from, to, rule, reason);
        if result == Added::Cycle && flag {
            let message = format!(
                "{} must come before {} by rule `{}`, but an earlier rule already puts it after",
                self.ids[from],
                self.ids[to],
                rule.as_str()
            );
            self.ask(
                to,
                "edge-conflict",
                message,
                "the earlier rule wins; add a Depends-On to the PR that should go first if the order is wrong",
            );
        }
    }

    fn apply_truncated_files(&mut self) {
        for i in 0..self.prs.len() {
            if !self.prs[i].files_complete {
                self.ask(
                    i,
                    "files-truncated",
                    "the PR has more changed files than were read, so package and DDL detection may be incomplete".to_string(),
                    "check the diff by hand or add the dependency with Depends-On",
                );
            }
        }
    }

    fn apply_depends_on(&mut self, external: &BTreeMap<String, DepState>) {
        for i in 0..self.prs.len() {
            let own_slug = self.prs[i].key.slug();
            let deps = detect::depends_on(&self.prs[i].body, &own_slug);
            for dep in deps {
                match dep {
                    Dep::Invalid(token) => self.ask(
                        i,
                        "depends-on-invalid",
                        format!("cannot read Depends-On value `{token}`"),
                        "write it as org/repo#123",
                    ),
                    Dep::Pr(key) => {
                        let id = key.lookup_id();
                        match self.lookup.get(&id).copied() {
                            Some(j) if j == i => self.ask(
                                i,
                                "depends-on-invalid",
                                "Depends-On points to the PR itself".to_string(),
                                "remove the line",
                            ),
                            Some(j) => {
                                let reason = format!("{} lists {} in Depends-On", self.ids[i], key);
                                self.link(j, i, Rule::DependsOn, reason, true);
                            }
                            None => self.external_dependency(i, &key, external.get(&id).copied()),
                        }
                    }
                }
            }
        }
    }

    fn external_dependency(&mut self, idx: usize, key: &PrKey, state: Option<DepState>) {
        match state {
            Some(DepState::Merged) => self.notes.push(format!(
                "{} depends on {}, already merged",
                self.ids[idx], key
            )),
            Some(DepState::Open) => self.ask(
                idx,
                "depends-on-open",
                format!("waits for {key}, which is open but not in this plan (not ready, or outside the train)"),
                "get that PR ready and add it to the plan, or remove the Depends-On",
            ),
            Some(DepState::Closed) => self.ask(
                idx,
                "depends-on-closed",
                format!("waits for {key}, which was closed without merging"),
                "fix or remove the Depends-On",
            ),
            Some(DepState::Missing) => self.ask(
                idx,
                "depends-on-missing",
                format!("waits for {key}, which could not be found"),
                "check the reference, or that the token can read that repo",
            ),
            None => self.ask(
                idx,
                "depends-on-not-checked",
                format!("waits for {key}, whose state was not checked because the plan lists too many outside references"),
                "check that PR by hand, or plan fewer PRs at once",
            ),
        }
    }

    fn apply_packages(&mut self, scanned: &Publishers) {
        let mut publishers: BTreeMap<String, BTreeSet<String>> = scanned.clone();
        for (package, slug) in &self.rules.packages {
            publishers.insert(package.clone(), BTreeSet::from([slug.clone()]));
        }
        for i in 0..self.prs.len() {
            let own = self.prs[i].key.slug().to_lowercase();
            for package in detect::bumped_packages(&self.prs[i].files) {
                let Some(repos) = publishers.get(&package) else {
                    continue;
                };
                let others: Vec<&String> = repos.iter().filter(|r| **r != own).collect();
                if others.len() > 1 {
                    let names: Vec<&str> = others.iter().map(|s| s.as_str()).collect();
                    self.ask(
                        i,
                        "package-ambiguous",
                        format!(
                            "bumps {package}, published by more than one repo: {}",
                            names.join(", ")
                        ),
                        "pin the publisher under [packages] in the config",
                    );
                }
                for repo in others {
                    let targets = self.by_repo.get(repo.as_str()).cloned().unwrap_or_default();
                    for j in targets {
                        let reason =
                            format!("{} bumps {package}, published by {repo}", self.ids[i]);
                        self.link(j, i, Rule::Package, reason, true);
                    }
                }
            }
        }
    }

    fn apply_stacked(&mut self) {
        for members in self.by_repo.values().cloned().collect::<Vec<_>>() {
            for &i in &members {
                for &j in &members {
                    if i != j && self.prs[j].base_ref == self.prs[i].head_ref {
                        let reason = format!(
                            "{} merges into `{}`, the branch of {}",
                            self.ids[j], self.prs[i].head_ref, self.ids[i]
                        );
                        self.link(j, i, Rule::Stacked, reason, true);
                    }
                }
            }
        }
    }

    fn order_group(&self, members: &[usize]) -> Vec<usize> {
        let mut remaining: Vec<usize> = members.to_vec();
        remaining.sort_by_key(|&i| (self.gates[i].is_none(), self.prs[i].key.number, i));
        let mut ordered = Vec::new();
        while !remaining.is_empty() {
            let pick = remaining
                .iter()
                .position(|&r| {
                    !remaining
                        .iter()
                        .any(|&m| m != r && self.graph.reaches(m, r))
                })
                .unwrap_or(0);
            ordered.push(remaining.remove(pick));
        }
        ordered
    }

    fn apply_targets(&mut self) {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in 0..self.prs.len() {
            groups
                .entry(self.rules.target_of(&self.prs[i]).to_lowercase())
                .or_default()
                .push(i);
        }
        for (target, members) in groups {
            if members.len() < 2 {
                continue;
            }
            let order = self.order_group(&members);
            for pair in order.windows(2) {
                self.link(
                    pair[0],
                    pair[1],
                    Rule::Target,
                    format!("same deploy target {target}"),
                    false,
                );
            }
        }
    }

    fn layer_name(&self, idx: usize) -> String {
        self.rules
            .layer_of(&self.prs[idx].key.slug())
            .map(|(_, name)| name.to_string())
            .unwrap_or_default()
    }

    fn apply_layers(&mut self) {
        let mut sets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in 0..self.prs.len() {
            sets.entry(self.prs[i].head_ref.clone())
                .or_default()
                .push(i);
        }
        for (branch, members) in sets {
            let repos: HashSet<String> = members
                .iter()
                .map(|&i| self.prs[i].key.slug().to_lowercase())
                .collect();
            if repos.len() < 2 {
                continue;
            }
            let mut ranked: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for &i in &members {
                match self.rules.layer_of(&self.prs[i].key.slug()) {
                    Some((rank, _)) => ranked.entry(rank).or_default().push(i),
                    None => {
                        let slug = self.prs[i].key.slug();
                        self.ask(
                            i,
                            "layer-unknown",
                            format!(
                                "{slug} is not in any layer, so its place on branch `{branch}` is undecided"
                            ),
                            "add the repo to a [[layer]] in the config",
                        );
                    }
                }
            }
            let groups: Vec<Vec<usize>> = ranked.into_values().collect();
            for pair in groups.windows(2) {
                for &lower in &pair[0] {
                    for &higher in &pair[1] {
                        let reason = format!(
                            "layer {} goes before layer {} on branch `{branch}`",
                            self.layer_name(lower),
                            self.layer_name(higher)
                        );
                        let flag = self.gates[lower].is_none() && self.gates[higher].is_none();
                        self.link(lower, higher, Rule::Layer, reason, flag);
                    }
                }
            }
        }
    }

    fn apply_gates(&mut self) {
        for g in 0..self.prs.len() {
            let Some(kind) = self.gates[g] else {
                continue;
            };
            let label = match kind {
                GateKind::Ddl => "DDL",
                GateKind::Infra => "infra",
            };
            for p in 0..self.prs.len() {
                if self.gates[p].is_some() {
                    continue;
                }
                self.link(
                    g,
                    p,
                    Rule::Gate,
                    format!("{label} goes first and pauses for confirmation"),
                    false,
                );
            }
        }
    }
}

pub fn plan(input: PlanInput<'_>) -> Planned {
    let rules = input.rules;
    let mut tagged = input.prs;
    tagged.sort_by(|a, b| a.0.key.cmp(&b.0.key));
    let scopes: Vec<String> = tagged.iter().map(|(_, s)| s.clone()).collect();
    let prs: Vec<Pr> = tagged.into_iter().map(|(p, _)| p).collect();
    let gate_info: Vec<Option<(GateKind, String)>> = prs.iter().map(|p| rules.gate_of(p)).collect();
    let gates: Vec<Option<GateKind>> = gate_info
        .iter()
        .map(|g| g.as_ref().map(|(kind, _)| *kind))
        .collect();

    let mut b = Builder::new(rules, prs);
    b.apply_truncated_files();
    b.apply_depends_on(&input.external);
    b.apply_packages(&input.publishers);
    b.apply_stacked();
    b.apply_order();
    b.apply_dependency(&input.references, &input.publishers);
    b.apply_targets();
    b.apply_gates();
    b.apply_layers();

    let count = b.prs.len();
    let stages = b.graph.stages();

    let mut order: Vec<usize> = (0..count).filter(|&i| gates[i].is_none()).collect();
    order.sort_by_key(|&i| (stages[i], i));
    let mut lanes: Vec<Vec<usize>> = Vec::new();
    let mut lane_of: Vec<Option<usize>> = vec![None; count];
    for i in order {
        let preds: Vec<usize> = b.graph.preds[i]
            .iter()
            .filter(|e| e.rule != Rule::Gate)
            .map(|e| e.from)
            .collect();
        let slot = lanes
            .iter()
            .position(|lane| lane.last().is_some_and(|tail| preds.contains(tail)));
        let lane = match slot {
            Some(k) => {
                lanes[k].push(i);
                k
            }
            None => {
                lanes.push(vec![i]);
                lanes.len() - 1
            }
        };
        lane_of[i] = Some(lane);
    }

    let mut width: HashMap<usize, usize> = HashMap::new();
    for i in (0..count).filter(|&i| gates[i].is_none()) {
        *width.entry(stages[i]).or_default() += 1;
    }
    let peak_parallel = width.values().copied().max().unwrap_or(0);

    let items: Vec<Item> = (0..count)
        .map(|i| {
            let pr = &b.prs[i];
            let after = b.graph.preds[i]
                .iter()
                .map(|e| {
                    let implied = b.graph.preds[i]
                        .iter()
                        .any(|other| other.from != e.from && b.graph.reaches(e.from, other.from));
                    Dependency {
                        id: b.ids[e.from].clone(),
                        rule: e.rule,
                        reason: e.reason.clone(),
                        implied,
                    }
                })
                .collect();
            let layer = rules.layer_of(&pr.key.slug());
            Item {
                id: b.ids[i].clone(),
                org: pr.key.org.clone(),
                repo: pr.key.repo.clone(),
                number: pr.key.number,
                title: pr.title.clone(),
                url: pr.url.clone(),
                head: pr.head_ref.clone(),
                base: pr.base_ref.clone(),
                sha: pr.head_sha.clone(),
                layer: layer.map(|(_, n)| n.to_string()),
                layer_rank: layer.map(|(rank, _)| rank),
                scope: scopes[i].clone(),
                gate: gates[i],
                gate_reason: gate_info[i].as_ref().map(|(_, reason)| reason.clone()),
                manual_pause: gates[i].is_some(),
                stage: stages[i],
                lane: lane_of[i].map(|l| l + 1),
                after,
            }
        })
        .collect();

    let lane_ids = lanes
        .iter()
        .map(|lane| lane.iter().map(|&i| b.ids[i].clone()).collect())
        .collect();
    let mut gate_order: Vec<usize> = (0..count).filter(|&i| gates[i].is_some()).collect();
    gate_order.sort_by_key(|&i| (stages[i], i));
    for &g in &gate_order {
        let blocker = b.graph.preds[g]
            .iter()
            .find(|e| gates[e.from].is_none())
            .map(|e| (e.from, e.rule));
        if let Some((from, rule)) = blocker {
            let message = format!(
                "{} is a gate but must come after {} by rule `{}`, so it cannot run first",
                b.ids[g],
                b.ids[from],
                rule.as_str()
            );
            b.ask(
                g,
                "gate-has-predecessor",
                message,
                "confirm the DDL or infra change can run after that PR, or split it out",
            );
        }
    }
    let gate_ids = gate_order.iter().map(|&i| b.ids[i].clone()).collect();

    Planned {
        items,
        lanes: lane_ids,
        gate: gate_ids,
        questions: b.questions,
        notes: b.notes,
        peak_parallel,
    }
}

pub fn external_refs(prs: &[Pr]) -> Vec<PrKey> {
    let inside: HashSet<String> = prs.iter().map(|p| p.key.lookup_id()).collect();
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for pr in prs {
        for dep in detect::depends_on(&pr.body, &pr.key.slug()) {
            if let Dep::Pr(key) = dep {
                let id = key.lookup_id();
                if !inside.contains(&id) && seen.insert(id) {
                    refs.push(key);
                }
            }
        }
    }
    refs
}
