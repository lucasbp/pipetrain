use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use crate::config::GateKind;
use crate::plan::Plan;
use crate::planner::{Item, Rule};
use crate::style::{bold, dim, green, paint, red, yellow};

fn heading(color: &str, title: &str, note: &str) -> String {
    format!("{}  {}", paint(&format!("1;{color}"), title), dim(note))
}

fn layer_paint(layer: &str, text: &str) -> String {
    let code = match layer {
        "lib" => "35",
        "client" => "34",
        "api" => "32",
        "bff" => "33",
        "front" => "36",
        _ => "2",
    };
    paint(code, text)
}

fn scope_code(scope: &str) -> &'static str {
    match scope {
        "train" => "32",
        "into-train" => "34",
        "shared-branch" => "35",
        "released" => "36",
        _ => "37",
    }
}

fn scope_label(scope: &str) -> &str {
    match scope {
        "shared-branch" => "shared",
        other => other,
    }
}

fn scope_paint(scope: &str, text: &str) -> String {
    paint(scope_code(scope), text)
}

fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(len)))
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

struct Names {
    prefix: Option<String>,
}

impl Names {
    fn new(plan: &Plan) -> Self {
        let prefix = match plan.orgs.as_slice() {
            [only] => Some(format!("{}/", only.to_lowercase())),
            _ => None,
        };
        Self { prefix }
    }

    fn short(&self, id: &str) -> String {
        match &self.prefix {
            Some(prefix) if id.to_lowercase().starts_with(prefix) => id[prefix.len()..].to_string(),
            _ => id.to_string(),
        }
    }
}

fn criterion(scope: &str, deploy_branch: &str) -> String {
    match scope {
        "train" => format!("head is {deploy_branch}"),
        "into-train" => format!("PR targets {deploy_branch}"),
        "shared-branch" => "same head branch in 2 or more repos".to_string(),
        "released" => "approved PR on any branch (--all)".to_string(),
        "explicit" => "named on the command line".to_string(),
        other => other.to_string(),
    }
}

struct Widths {
    id: usize,
    layer: usize,
    scope: usize,
}

fn widths(plan: &Plan, names: &Names) -> Widths {
    Widths {
        id: plan
            .items
            .iter()
            .map(|i| names.short(&i.id).chars().count())
            .max()
            .unwrap_or(0),
        layer: plan
            .items
            .iter()
            .filter_map(|i| i.layer.as_deref())
            .map(|l| l.chars().count() + 2)
            .max()
            .unwrap_or(0),
        scope: plan
            .items
            .iter()
            .map(|i| scope_label(&i.scope).chars().count())
            .max()
            .unwrap_or(0),
    }
}

fn item_line(out: &mut String, item: &Item, names: &Names, w: &Widths, marked: bool, stage: bool) {
    let mark = if marked { red("?") } else { " ".to_string() };
    let step = if stage {
        dim(&format!("s{} ", item.stage))
    } else {
        String::new()
    };
    let layer_text = item
        .layer
        .as_deref()
        .map(|l| format!("[{l}]"))
        .unwrap_or_default();
    let layer = match item.layer.as_deref() {
        Some(l) => layer_paint(l, &pad(&layer_text, w.layer)),
        None => " ".repeat(w.layer),
    };
    let scope = scope_paint(&item.scope, &pad(scope_label(&item.scope), w.scope));
    let _ = writeln!(
        out,
        "  {step}{mark} {}  {layer} {scope}  {}",
        bold(&pad(&names.short(&item.id), w.id)),
        clip(&item.title, 52)
    );
}

fn after_lines(out: &mut String, item: &Item, names: &Names, indent: &str) {
    for dep in item
        .after
        .iter()
        .filter(|d| !d.implied && d.rule != Rule::Gate)
    {
        let _ = writeln!(
            out,
            "{indent}{} {}  {}",
            dim("after"),
            bold(&names.short(&dep.id)),
            dim(&format!("({}: {})", dep.rule.as_str(), dep.reason))
        );
    }
}

fn origin_section(out: &mut String, plan: &Plan, names: &Names) {
    let scopes = [
        "train",
        "into-train",
        "shared-branch",
        "released",
        "explicit",
    ];
    let present: Vec<&str> = scopes
        .into_iter()
        .filter(|s| {
            plan.items.iter().any(|i| i.scope == *s) || plan.skipped.iter().any(|k| k.scope == *s)
        })
        .collect();
    if present.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n{}",
        heading("36", "WHERE IT CAME FROM", "how each PR got into the queue")
    );
    for scope in present {
        let ready: Vec<&Item> = plan.items.iter().filter(|i| i.scope == scope).collect();
        let skipped = plan.skipped.iter().filter(|s| s.scope == scope).count();
        let counts = if skipped > 0 {
            format!(
                "{} ready, {}",
                ready.len(),
                yellow(&format!("{skipped} skipped"))
            )
        } else {
            format!("{} ready", ready.len())
        };
        let _ = writeln!(
            out,
            "  {} {}  {}  {}",
            scope_paint(scope, "●"),
            scope_paint(scope, &pad(scope_label(scope), 6)),
            counts,
            dim(&criterion(scope, &plan.deploy_branch))
        );
        if scope == "shared-branch" {
            let mut by_branch: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            for item in &ready {
                by_branch
                    .entry(item.head.as_str())
                    .or_default()
                    .push(names.short(&item.id));
            }
            for (branch, ids) in by_branch {
                let _ = writeln!(
                    out,
                    "        {}  {}",
                    bold(branch),
                    ids.join(dim(" · ").as_str())
                );
            }
        }
    }
}

fn order_section(out: &mut String, plan: &Plan, names: &Names, by_id: &HashMap<&str, &Item>) {
    if plan.items.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n{}",
        heading(
            "36",
            "ORDER",
            "a step starts when every PR of the step above is merged and deployed"
        )
    );
    let sep = dim(" · ");
    if !plan.gate.is_empty() {
        let cells: Vec<String> = plan
            .gate
            .iter()
            .filter_map(|id| by_id.get(id.as_str()))
            .map(|item| {
                let kind = match item.gate {
                    Some(GateKind::Ddl) => "DDL",
                    Some(GateKind::Infra) => "infra",
                    None => "",
                };
                format!("{} {}", bold(&names.short(&item.id)), yellow(kind))
            })
            .collect();
        let _ = writeln!(
            out,
            "  {} {}  {}",
            red("pause"),
            cells.join(&sep),
            dim("(you confirm first)")
        );
    }
    let mut stages: BTreeMap<usize, Vec<&Item>> = BTreeMap::new();
    for item in plan.items.iter().filter(|i| !i.manual_pause) {
        stages.entry(item.stage).or_default().push(item);
    }
    for (stage, items) in stages {
        let cells: Vec<String> = items
            .iter()
            .map(|i| {
                let id = names.short(&i.id);
                match i.layer.as_deref() {
                    Some(l) => layer_paint(l, &id),
                    None => id,
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "  {} {}  {}",
            dim("step"),
            bold(&format!("{stage}")),
            cells.join(&sep)
        );
    }
}

pub fn text(plan: &Plan) -> String {
    let mut out = String::new();
    let names = Names::new(plan);
    let w = widths(plan, &names);
    let by_id: HashMap<&str, &Item> = plan.items.iter().map(|i| (i.id.as_str(), i)).collect();
    let marked: HashMap<&str, usize> = plan.questions.iter().filter_map(|q| q.pr.as_deref()).fold(
        HashMap::new(),
        |mut acc, id| {
            *acc.entry(id).or_default() += 1;
            acc
        },
    );

    let _ = writeln!(
        out,
        "{}  {}",
        bold("pipetrain plan"),
        dim("(read-only, nothing was merged)")
    );
    let _ = writeln!(out, "  {} {}", dim("train    "), plan.train);
    if let Some(info) = &plan.profile {
        let _ = writeln!(
            out,
            "  {} {} ({})",
            dim("profile  "),
            info.name,
            info.source.label()
        );
    }
    let _ = writeln!(out, "  {} {}", dim("orgs     "), plan.orgs.join(", "));
    let skipped = if plan.skipped.is_empty() {
        format!("{} skipped", plan.skipped.len())
    } else {
        yellow(&format!("{} skipped", plan.skipped.len()))
    };
    let questions = if plan.questions.is_empty() {
        format!("{} questions", plan.questions.len())
    } else {
        red(&format!("{} questions", plan.questions.len()))
    };
    let _ = writeln!(
        out,
        "  {} {}, {}, {}",
        dim("found    "),
        green(&format!("{} ready", plan.items.len())),
        skipped,
        questions
    );
    let _ = writeln!(
        out,
        "  {} peak {} at once (cap {})",
        dim("parallel "),
        plan.stats.peak_parallel,
        plan.stats.max_parallel
    );
    let saved = if plan.stats.deploys_saved > 0 {
        green(&format!("{} after bundling", plan.stats.deploys_after))
    } else {
        format!("{} after bundling", plan.stats.deploys_after)
    };
    let _ = writeln!(
        out,
        "  {} {} now, {}",
        dim("deploys  "),
        plan.stats.deploys_now,
        saved
    );
    let _ = writeln!(
        out,
        "  {} {}",
        dim("api      "),
        dim(&format!(
            "{} requests, {} left",
            plan.stats.requests,
            plan.stats
                .rate_remaining
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string())
        ))
    );

    if plan.items.is_empty() {
        let _ = writeln!(out, "\n{}", yellow("Nothing ready to plan."));
        if plan.train != "explicit PRs" && plan.train != "all released PRs" {
            let _ = writeln!(
                out,
                "Tip: nothing is on {} today. Run with --all to plan every approved PR across the orgs.",
                plan.train
            );
        }
    }

    origin_section(&mut out, plan, &names);
    order_section(&mut out, plan, &names, &by_id);

    if !plan.gate.is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            heading("31", "GATE", "runs first, pauses for your confirmation")
        );
        for id in &plan.gate {
            if let Some(item) = by_id.get(id.as_str()) {
                item_line(
                    &mut out,
                    item,
                    &names,
                    &w,
                    marked.contains_key(id.as_str()),
                    false,
                );
                let kind = match item.gate {
                    Some(GateKind::Ddl) => "DDL",
                    Some(GateKind::Infra) => "infra",
                    None => "",
                };
                let reason = item.gate_reason.as_deref().unwrap_or("");
                let _ = writeln!(
                    out,
                    "        {} {}  {}",
                    yellow(kind),
                    dim(&format!("({reason})")),
                    dim("every lane below waits for this")
                );
                after_lines(&mut out, item, &names, "        ");
            }
        }
    }

    for (n, lane) in plan.lanes.iter().enumerate() {
        let _ = writeln!(out, "\n{}", paint("1;36", &format!("LANE {}", n + 1)));
        for id in lane {
            if let Some(item) = by_id.get(id.as_str()) {
                item_line(
                    &mut out,
                    item,
                    &names,
                    &w,
                    marked.contains_key(id.as_str()),
                    true,
                );
                after_lines(&mut out, item, &names, "            ");
            }
        }
    }

    if !plan.bundles.is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            heading(
                "32",
                "BUNDLES",
                "one deploy branch instead of N deploys (suggestion, nothing was changed)"
            )
        );
        for b in &plan.bundles {
            let ids = b
                .retarget
                .iter()
                .map(|id| names.short(id))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "  {}  {} PRs into {} -> 1 deploy, {}",
                bold(&names.short(&b.repo)),
                b.deploys_now,
                b.base,
                green(&format!("saves {}", b.saved))
            );
            match &b.existing_train_pr {
                None => {
                    let _ = writeln!(
                        out,
                        "        {}",
                        dim(&format!(
                            "create {} from {}, retarget {ids}, merge them there, then open one PR {} -> {}",
                            b.branch, b.base, b.branch, b.base
                        ))
                    );
                }
                Some(existing) => {
                    let _ = writeln!(
                        out,
                        "        {}",
                        dim(&format!(
                            "retarget {ids} to {} and merge them there; {} then carries all of them",
                            b.branch,
                            names.short(existing)
                        ))
                    );
                }
            }
        }
    }

    if !plan.skipped.is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            heading("33", "SKIPPED", "not ready, left out of the plan")
        );
        for s in &plan.skipped {
            let _ = writeln!(
                out,
                "  {}  {}  {}  {}",
                bold(&names.short(&s.id)),
                dim(&clip(&s.title, 40)),
                scope_paint(
                    &s.scope,
                    &format!("[{}: {}]", scope_label(&s.scope), clip(&s.head, 32))
                ),
                yellow(&format!("({})", s.reasons.join(", ")))
            );
        }
    }

    if !plan.questions.is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            heading("31", "?  QUESTIONS", "the planner could not decide these")
        );
        for q in &plan.questions {
            let who = q.pr.as_deref().map(|id| names.short(id));
            let _ = writeln!(
                out,
                "  {}  {}  {}",
                bold(who.as_deref().unwrap_or("-")),
                red(&format!("[{}]", q.code)),
                q.message
            );
            let _ = writeln!(out, "        {} {}", dim("fix:"), q.hint);
        }
    }

    if !plan.notes.is_empty() {
        let _ = writeln!(out, "\n{}", bold("NOTES"));
        for note in &plan.notes {
            let _ = writeln!(out, "  {}", dim(note));
        }
    }
    out
}
