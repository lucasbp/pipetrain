use std::collections::BTreeMap;

mod common;

use common::pr;
use pipetrain::command::merge::bump;
use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::merge::{Pending, Store, StoreOperation};
use pipetrain::model::Pr;
use pipetrain::plan::{self, Gathered};
use pipetrain::planner::{self, PlanInput, Planned};
use pipetrain::publishers::{self, Publishers, References};

type Manifest = (String, String, String);

fn config(extra: &str) -> Config {
    Config::parse(&format!("orgs = [\"acme\"]\n{extra}\n")).unwrap()
}

fn manifest(slug: &str, path: &str, text: &str) -> Manifest {
    (slug.to_string(), path.to_string(), text.to_string())
}

fn scan(manifests: &[Manifest]) -> (Publishers, References) {
    (
        publishers::collect(manifests),
        publishers::collect_references(manifests),
    )
}

fn util_and_sdk() -> Vec<Manifest> {
    vec![
        manifest(
            "acme/util",
            "src/Acme.Util.csproj",
            "<Project><PropertyGroup><GeneratePackageOnBuild>true</GeneratePackageOnBuild></PropertyGroup></Project>",
        ),
        manifest(
            "acme/sdk",
            "src/Acme.Sdk.csproj",
            r#"<Project><ItemGroup><PackageReference Include="Acme.Util" Version="1.0.0" /></ItemGroup></Project>"#,
        ),
    ]
}

fn run(cfg: &Config, prs: Vec<Pr>, manifests: &[Manifest]) -> Planned {
    let rules = Rules::new(cfg).unwrap();
    let (publishers, references) = scan(manifests);
    planner::plan(PlanInput {
        rules: &rules,
        prs: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        external: BTreeMap::new(),
        publishers,
        references,
    })
}

fn edge(planned: &Planned, from: &str, to: &str, rule: &str) -> bool {
    planned
        .items
        .iter()
        .find(|i| i.id == to)
        .unwrap()
        .after
        .iter()
        .any(|d| d.id == from && d.rule.as_str() == rule)
}

fn edges_of(planned: &Planned, rule: &str) -> usize {
    planned
        .items
        .iter()
        .flat_map(|i| i.after.iter())
        .filter(|d| d.rule.as_str() == rule)
        .count()
}

fn codes(planned: &Planned) -> Vec<&str> {
    planned.questions.iter().map(|q| q.code.as_str()).collect()
}

fn build(cfg: &Config, prs: Vec<Pr>, manifests: &[Manifest]) -> plan::Plan {
    plan::build(&Rules::new(cfg).unwrap(), gathered(prs, manifests))
}

fn gathered(prs: Vec<Pr>, manifests: &[Manifest]) -> Gathered {
    let (publishers, references) = scan(manifests);
    Gathered {
        generated_at: "2026-10-30T09:00:00-03:00".to_string(),
        train: "deploy/30-10-2026".to_string(),
        deploy_branch: "deploy/30-10-2026".to_string(),
        train_prefix: "deploy/".to_string(),
        orgs: vec!["acme".to_string()],
        ready: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        skipped: Vec::new(),
        questions: Vec::new(),
        external: BTreeMap::new(),
        publishers,
        references,
        max_parallel: 4,
        requests: 1,
        rate_remaining: Some(4000),
    }
}

fn queue(cfg: &Config, prs: Vec<Pr>, manifests: &[Manifest]) -> Vec<String> {
    emit::queue_lines(&build(cfg, prs, manifests))
}

fn position(lines: &[String], id: &str) -> usize {
    lines
        .iter()
        .position(|line| line == &format!("merge:{id}"))
        .unwrap()
}

fn util_and_sdk_prs() -> Vec<Pr> {
    vec![
        pr("acme/sdk", 1, "feat/a", "master"),
        pr("acme/util", 2, "feat/b", "master"),
    ]
}

fn record_util_merge(plan: &plan::Plan) -> Store {
    let mut store = Store::in_memory();
    bump::track(
        plan,
        &mut store,
        &bump::Options::default(),
        &StoreOperation::Add(Pending {
            recorded_at: None,
            id: "acme/util#2".into(),
            repo: "acme/util".into(),
            base: "master".into(),
            sha: None,
            admin_override: None,
            continuation: None,
            deploy_prediction: None,
        }),
    );
    store
}

#[test]
fn resuming_a_held_publisher_retains_the_package_contract_for_its_selected_dependent() {
    use pipetrain::discovery::{Scope, Skipped};
    let mut initial = gathered(util_and_sdk_prs(), &util_and_sdk());
    let index = initial
        .ready
        .iter()
        .position(|(pr, _)| pr.key.repo == "util")
        .unwrap();
    let (publisher, _) = initial.ready.remove(index);
    initial.skipped.push(Skipped {
        pr: publisher,
        scope: Scope::Explicit,
        reasons: vec!["waiting: needs a bump of acme.core".into()],
    });
    let plan = plan::build(&Rules::new(&config("")).unwrap(), initial);
    assert!(plan.items.is_empty());
    let store = record_util_merge(&plan);
    let journal = store.state.bump.unwrap();
    assert_eq!(journal.publishers["acme/util#2"].packages, ["acme.util"]);
    assert_eq!(
        journal.contracts["acme/sdk#1"][0].publisher_id,
        "acme/util#2"
    );
}

fn assert_package_survives_winning_rule(cfg: &Config, prs: Vec<Pr>, rule: &str) {
    let plan = build(cfg, prs, &util_and_sdk());
    let consumer = plan
        .items
        .iter()
        .find(|item| item.id == "acme/sdk#1")
        .unwrap();
    let contracts: Vec<_> = consumer
        .after
        .iter()
        .filter(|dep| dep.id == "acme/util#2")
        .collect();
    assert_eq!(contracts.len(), 1);
    assert_eq!(contracts[0].rule.as_str(), rule);
    assert_eq!(contracts[0].package.as_deref(), Some("acme.util"));
    assert!(bump::applicable(consumer, None));
    let lines = emit::queue_lines(&plan);
    assert!(position(&lines, "acme/util#2") < position(&lines, "acme/sdk#1"));
    let store = record_util_merge(&plan);
    let journal = store.state.bump.unwrap();
    assert_eq!(journal.publishers["acme/util#2"].packages, ["acme.util"]);
    assert_eq!(journal.contracts[&consumer.id].len(), 1);
    assert_eq!(journal.contracts[&consumer.id][0].package, "acme.util");
    assert_eq!(
        journal.contracts[&consumer.id][0].publisher_id,
        "acme/util#2"
    );
}

#[test]
fn explicit_depends_on_keeps_its_rule_and_package_for_consumer_bumps() {
    let mut prs = util_and_sdk_prs();
    prs[0].body = "Depends-On: acme/util#2".into();
    assert_package_survives_winning_rule(&config(""), prs, "depends-on");
}

#[test]
fn explicit_config_order_keeps_its_rule_and_package_for_consumer_bumps() {
    let cfg = config("[[order]]\nrepos = [\"acme/util\", \"acme/sdk\"]\n");
    assert_package_survives_winning_rule(&cfg, util_and_sdk_prs(), "order");
}

#[test]
fn a_consumer_reference_orders_the_publisher_first_with_the_reason() {
    let planned = run(&config(""), util_and_sdk_prs(), &util_and_sdk());
    assert!(edge(&planned, "acme/util#2", "acme/sdk#1", "dependency"));
    assert!(!edge(&planned, "acme/sdk#1", "acme/util#2", "dependency"));
    let dep = planned
        .items
        .iter()
        .find(|i| i.id == "acme/sdk#1")
        .unwrap()
        .after
        .iter()
        .find(|d| d.rule.as_str() == "dependency")
        .unwrap();
    assert_eq!(
        dep.reason,
        "sdk references acme.util, published by util (kept by doubt: the feed of acme.util could not be read)"
    );
    assert!(codes(&planned).is_empty());
}

#[test]
fn no_reference_means_no_edge() {
    let manifests = vec![
        util_and_sdk().remove(0),
        manifest(
            "acme/sdk",
            "src/Acme.Sdk.csproj",
            r#"<Project><ItemGroup><PackageReference Include="Newtonsoft.Json" Version="13.0.1" /></ItemGroup></Project>"#,
        ),
    ];
    let planned = run(&config(""), util_and_sdk_prs(), &manifests);
    assert_eq!(edges_of(&planned, "dependency"), 0);
    assert!(codes(&planned).is_empty());
}

#[test]
fn a_plan_with_no_references_is_unchanged() {
    let cfg = config("");
    let with_publishers: Vec<Manifest> = util_and_sdk().into_iter().take(1).collect();
    let plain = run(&cfg, util_and_sdk_prs(), &[]);
    let scanned = run(&cfg, util_and_sdk_prs(), &with_publishers);
    assert_eq!(
        serde_json::to_string(&plain.items).unwrap(),
        serde_json::to_string(&scanned.items).unwrap()
    );
    assert_eq!(codes(&plain), codes(&scanned));
}

#[test]
fn an_npm_publisher_goes_before_the_repo_that_depends_on_it() {
    let manifests = vec![
        manifest("acme/kit", "package.json", r#"{"name":"@acme/kit"}"#),
        manifest(
            "acme/web",
            "package.json",
            r#"{"name":"web","dependencies":{"@acme/kit":"^1.0.0"}}"#,
        ),
    ];
    let prs = vec![
        pr("acme/web", 1, "feat/a", "master"),
        pr("acme/kit", 2, "feat/b", "master"),
    ];
    let planned = run(&config(""), prs, &manifests);
    assert!(edge(&planned, "acme/kit#2", "acme/web#1", "dependency"));
}

#[test]
fn a_project_marked_not_packable_publishes_nothing() {
    let manifests = vec![
        manifest(
            "acme/util",
            "src/Acme.Util.csproj",
            "<Project><PropertyGroup><IsPackable>false</IsPackable><GeneratePackageOnBuild>true</GeneratePackageOnBuild></PropertyGroup></Project>",
        ),
        util_and_sdk().remove(1),
    ];
    let (publishers, _) = scan(&manifests);
    assert!(publishers.is_empty());
    let planned = run(&config(""), util_and_sdk_prs(), &manifests);
    assert_eq!(edges_of(&planned, "dependency"), 0);
}

#[test]
fn a_test_project_publishes_nothing() {
    let manifests = vec![
        manifest(
            "acme/util",
            "tests/Acme.Util.Tests.csproj",
            "<Project><PropertyGroup><GeneratePackageOnBuild>true</GeneratePackageOnBuild></PropertyGroup></Project>",
        ),
        manifest(
            "acme/sdk",
            "src/Acme.Sdk.csproj",
            r#"<Project><ItemGroup><PackageReference Include="Acme.Util.Tests" Version="1.0.0" /></ItemGroup></Project>"#,
        ),
    ];
    let (publishers, _) = scan(&manifests);
    assert!(publishers.is_empty());
    let planned = run(&config(""), util_and_sdk_prs(), &manifests);
    assert_eq!(edges_of(&planned, "dependency"), 0);
}

#[test]
fn mutual_references_ask_and_add_no_edge() {
    let manifests = vec![
        manifest(
            "acme/util",
            "src/Acme.Util.csproj",
            r#"<Project><PropertyGroup><GeneratePackageOnBuild>true</GeneratePackageOnBuild></PropertyGroup><ItemGroup><PackageReference Include="Acme.Sdk" Version="1.0.0" /></ItemGroup></Project>"#,
        ),
        manifest(
            "acme/sdk",
            "src/Acme.Sdk.csproj",
            r#"<Project><PropertyGroup><GeneratePackageOnBuild>true</GeneratePackageOnBuild></PropertyGroup><ItemGroup><PackageReference Include="Acme.Util" Version="1.0.0" /></ItemGroup></Project>"#,
        ),
    ];
    let planned = run(&config(""), util_and_sdk_prs(), &manifests);
    assert_eq!(edges_of(&planned, "dependency"), 0);
    assert_eq!(codes(&planned), vec!["dependency-cycle"]);
    let question = &planned.questions[0];
    assert!(question.pr.is_none());
    assert!(question.message.contains("acme/sdk"));
    assert!(question.message.contains("acme/util"));
}

#[test]
fn an_explicit_order_beats_dependency_and_the_conflict_is_asked() {
    let extra = "[[order]]\nrepos = [\"acme/sdk\", \"acme/util\"]\n";
    let planned = run(&config(extra), util_and_sdk_prs(), &util_and_sdk());
    assert!(edge(&planned, "acme/sdk#1", "acme/util#2", "order"));
    assert!(!edge(&planned, "acme/util#2", "acme/sdk#1", "dependency"));
    assert_eq!(codes(&planned), vec!["edge-conflict"]);
}

#[test]
fn the_queue_puts_the_publisher_first() {
    let lines = queue(&config(""), util_and_sdk_prs(), &util_and_sdk());
    assert!(position(&lines, "acme/util#2") < position(&lines, "acme/sdk#1"));
}

fn with_evidence(cfg: &Config, prs: Vec<Pr>, pins: &[&str], feed: Option<&[&str]>) -> Planned {
    let mut rules = Rules::new(cfg).unwrap();
    let (publishers, references) = scan(&util_and_sdk());
    rules.evidence.pins.insert(
        ("acme/sdk".to_string(), "acme.util".to_string()),
        pins.iter().map(|pin| pin.to_string()).collect(),
    );
    if let Some(versions) = feed {
        rules.evidence.feeds.insert(
            "acme.util".to_string(),
            versions.iter().map(|version| version.to_string()).collect(),
        );
    }
    planner::plan(PlanInput {
        rules: &rules,
        prs: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        external: BTreeMap::new(),
        publishers,
        references,
    })
}

fn reason_of(planned: &Planned, to: &str, rule: &str) -> String {
    planned
        .items
        .iter()
        .find(|i| i.id == to)
        .unwrap()
        .after
        .iter()
        .find(|d| d.rule.as_str() == rule)
        .unwrap()
        .reason
        .clone()
}

fn sdk_bumping_util() -> Pr {
    let mut sdk = pr("acme/sdk", 1, "feat/a", "master");
    sdk.files = vec![pipetrain::model::ChangedFile {
        path: "src/Acme.Sdk.csproj".to_string(),
        patch: Some(
            "-<PackageReference Include=\"Acme.Util\" Version=\"1.0.0\" />\n+<PackageReference Include=\"Acme.Util\" Version=\"1.0.1\" />"
                .to_string(),
        ),
    }];
    sdk
}

#[test]
fn a_pin_already_published_puts_the_consumer_before_the_publisher() {
    let planned = with_evidence(
        &config(""),
        util_and_sdk_prs(),
        &["1.0.0"],
        Some(&["0.9.0", "1.0.0"]),
    );
    assert!(edge(
        &planned,
        "acme/sdk#1",
        "acme/util#2",
        "package-before-publisher"
    ));
    assert_eq!(edges_of(&planned, "dependency"), 0);
    assert!(codes(&planned).is_empty());
    assert_eq!(
        reason_of(&planned, "acme/util#2", "package-before-publisher"),
        "sdk pins acme.util 1.0.0, already published; merging before util avoids a forced bump"
    );
}

#[test]
fn a_pin_not_yet_published_keeps_the_publisher_first() {
    let planned = with_evidence(
        &config(""),
        util_and_sdk_prs(),
        &["1.0.5"],
        Some(&["0.9.0", "1.0.0"]),
    );
    assert!(edge(&planned, "acme/util#2", "acme/sdk#1", "dependency"));
    assert_eq!(edges_of(&planned, "package-before-publisher"), 0);
    assert_eq!(
        reason_of(&planned, "acme/sdk#1", "dependency"),
        "sdk references acme.util 1.0.5, which is not published yet, so it waits for util"
    );
}

#[test]
fn a_consumer_that_bumps_the_package_keeps_the_package_edge_only() {
    let prs = vec![sdk_bumping_util(), pr("acme/util", 2, "feat/b", "master")];
    let planned = with_evidence(&config(""), prs, &["1.0.0"], Some(&["1.0.0"]));
    assert!(edge(&planned, "acme/util#2", "acme/sdk#1", "package"));
    assert_eq!(edges_of(&planned, "dependency"), 0);
    assert_eq!(edges_of(&planned, "package-before-publisher"), 0);
}

#[test]
fn an_unreadable_feed_keeps_the_old_edge_by_doubt() {
    let planned = with_evidence(&config(""), util_and_sdk_prs(), &["1.0.0"], None);
    assert!(edge(&planned, "acme/util#2", "acme/sdk#1", "dependency"));
    assert!(
        reason_of(&planned, "acme/sdk#1", "dependency")
            .ends_with("(kept by doubt: the feed of acme.util could not be read)")
    );
}

#[test]
fn a_range_without_a_pin_keeps_the_old_edge_by_doubt() {
    let planned = with_evidence(
        &config(""),
        util_and_sdk_prs(),
        &["[1.0,2.0)"],
        Some(&["1.0.0"]),
    );
    assert!(edge(&planned, "acme/util#2", "acme/sdk#1", "dependency"));
    assert!(
        reason_of(&planned, "acme/sdk#1", "dependency")
            .ends_with("(kept by doubt: version not resolved)")
    );
}

#[test]
fn a_held_dependent_names_the_rule_and_reason_in_the_json() {
    let mut sdk = pr("acme/sdk", 1, "feat/a", "master");
    sdk.body = "Depends-On: acme/util#2".into();
    let util = pr("acme/util", 2, "feat/b", "master");
    let cfg = config("");
    let rules = Rules::new(&cfg).unwrap();
    let plan = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: vec![(sdk, "explicit".to_string())],
            skipped: vec![pipetrain::discovery::Skipped {
                pr: util,
                scope: pipetrain::discovery::Scope::Explicit,
                reasons: vec!["checks failing".to_string()],
            }],
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: References::new(),
            max_parallel: 4,
            requests: 1,
            rate_remaining: Some(4000),
        },
    );
    let value = serde_json::to_value(&plan).unwrap();
    let held = value["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "acme/sdk#1")
        .unwrap();
    assert_eq!(held["blocked_by"][0]["id"], "acme/util#2");
    assert_eq!(held["blocked_by"][0]["rule"], "depends-on");
    assert!(!held["blocked_by"][0]["reason"].as_str().unwrap().is_empty());
    let failing = value["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "acme/util#2")
        .unwrap();
    assert!(failing.get("blocked_by").is_none());
}
