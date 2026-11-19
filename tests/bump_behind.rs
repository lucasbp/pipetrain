use std::collections::BTreeMap;

mod common;

use common::pr;
use pipetrain::config::{Config, Rules};
use pipetrain::discovery::Scope;
use pipetrain::merge::category::{Category, classify};
use pipetrain::model::{ChangedFile, Pr};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::planner::behind::{self, Feeds, Need};
use pipetrain::planner::release::Context;
use pipetrain::publishers::{self, Publishers, References};
use pipetrain::workflows::{self, MaxBehind};

type Manifest = (String, String, String);

fn manifest(slug: &str, path: &str, text: &str) -> Manifest {
    (slug.to_string(), path.to_string(), text.to_string())
}

fn manifests(pin: &str) -> Vec<Manifest> {
    vec![
        manifest(
            "acme/util",
            "src/Acme.Util.csproj",
            "<Project><PropertyGroup><GeneratePackageOnBuild>true</GeneratePackageOnBuild></PropertyGroup></Project>",
        ),
        manifest(
            "acme/sdk",
            "src/Acme.Sdk.csproj",
            &format!(
                r#"<Project><ItemGroup><PackageReference Include="Acme.Util" Version="{pin}" /></ItemGroup></Project>"#
            ),
        ),
    ]
}

fn touching(mut pr: Pr, path: &str) -> Pr {
    pr.files = vec![ChangedFile {
        path: path.to_string(),
        patch: None,
    }];
    pr
}

fn util_release(number: u64) -> Pr {
    touching(
        pr("acme/util", number, &format!("feat/u{number}"), "main"),
        "src/Feature.cs",
    )
}

fn consumer() -> Pr {
    touching(pr("acme/sdk", 10, "feat/s", "main"), "src/Client.cs")
}

fn web() -> Pr {
    let mut web = touching(pr("acme/web", 20, "feat/w", "main"), "src/App.cs");
    web.body = "Depends-On: acme/sdk#10".to_string();
    web
}

fn workflow(limit: &str) -> Vec<(String, String)> {
    vec![(
        ".github/workflows/check.yml".to_string(),
        format!(
            "name: check\njobs:\n  versions:\n    steps:\n      - run: check-versions --max-behind {limit}\n"
        ),
    )]
}

fn context(pin: &str, prs: &[&Pr], limit: Option<MaxBehind>, default_max: Option<u32>) -> Context {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let manifests = manifests(pin);
    let publishers: Publishers = publishers::collect(&manifests);
    let references: References = publishers::collect_references(&manifests);
    let mut context = Context::new(
        &rules,
        &publishers,
        &references,
        &manifests,
        prs,
        default_max,
    );
    if let Some(limit) = limit {
        context.limits.insert("acme/sdk".to_string(), limit);
    }
    context
}

fn limit(max: u32) -> Option<MaxBehind> {
    Some(workflows::max_behind(&workflow(&max.to_string())))
}

fn feed(versions: &[&str]) -> Feeds {
    BTreeMap::from([(
        "acme.util".to_string(),
        versions.iter().map(|v| (*v).to_string()).collect(),
    )])
}

fn ready(prs: Vec<Pr>) -> Vec<(Pr, Scope)> {
    prs.into_iter().map(|p| (p, Scope::Explicit)).collect()
}

fn needs(pin: &str, feeds: &Feeds, max: Option<MaxBehind>, prs: Vec<Pr>) -> Vec<Need> {
    let refs: Vec<&Pr> = prs.iter().collect();
    let context = context(pin, &refs, max, None);
    behind::find(&context, &ready(prs), feeds)
}

#[test]
fn a_pin_further_behind_the_feed_than_the_limit_needs_the_newest_stable_in_its_major() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0", "2.0.0"]);
    let found = needs("1.0.0", &feeds, limit(1), vec![consumer()]);
    assert_eq!(found.len(), 1);
    let need = &found[0];
    assert_eq!(need.id, "acme/sdk#10");
    assert_eq!(need.package, "acme.util");
    assert_eq!(need.from, "1.0.0");
    assert_eq!(need.to.as_deref(), Some("1.4.0"));
    assert_eq!(need.behind, 5);
    assert_eq!(need.limit, 1);
    assert!(!need.queue_published);
    assert_eq!(need.cause(), "needs a bump of acme.util to 1.4.0");
}

#[test]
fn a_prerelease_is_never_proposed() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0-develop", "1.3.0-rc.1"]);
    let found = needs("1.0.0", &feeds, limit(1), vec![consumer()]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].to.as_deref(), Some("1.1.0"));
}

#[test]
fn only_a_newer_major_or_prereleases_leaves_nothing_to_propose() {
    let feeds = feed(&["1.0.0", "2.0.0", "2.1.0", "1.1.0-beta"]);
    assert!(needs("1.0.0", &feeds, limit(1), vec![consumer()]).is_empty());
}

#[test]
fn a_pin_within_the_limit_is_left_alone() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0"]);
    assert!(needs("1.0.0", &feeds, limit(2), vec![consumer()]).is_empty());
}

#[test]
fn an_unreadable_limit_does_not_hold_the_pr() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    let unknown = Some(workflows::max_behind(&[]));
    assert!(needs("1.0.0", &feeds, unknown, vec![consumer()]).is_empty());
}

#[test]
fn the_config_default_limit_applies_when_the_workflows_do_not_say() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    let prs = vec![consumer()];
    let refs: Vec<&Pr> = prs.iter().collect();
    let context = context("1.0.0", &refs, Some(workflows::max_behind(&[])), Some(1));
    let found = behind::find(&context, &ready(prs), &feeds);
    assert_eq!(found.len(), 1);
}

#[test]
fn a_range_is_not_a_pin() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    assert!(needs("1.*", &feeds, limit(1), vec![consumer()]).is_empty());
}

#[test]
fn queue_releases_count_as_versions_the_pin_will_fall_behind() {
    let feeds = feed(&["1.0.0"]);
    let found = needs(
        "1.0.0",
        &feeds,
        limit(1),
        vec![
            util_release(1),
            util_release(2),
            util_release(3),
            consumer(),
        ],
    );
    assert_eq!(found.len(), 1);
    let need = &found[0];
    assert!(need.queue_published);
    assert_eq!(need.to, None);
    assert_eq!(need.behind, 3);
    assert_eq!(
        need.cause(),
        "needs a bump of acme.util to the version the queue publishes"
    );
}

#[test]
fn a_pin_ahead_of_the_feed_is_a_bump_to_what_the_queue_publishes() {
    let feeds = feed(&["1.0.0"]);
    let found = needs(
        "1.0.3",
        &feeds,
        limit(1),
        vec![
            util_release(1),
            util_release(2),
            util_release(3),
            consumer(),
        ],
    );
    assert!(found.is_empty());
}

#[test]
fn hold_moves_the_pr_out_of_the_queue_with_a_waiting_cause() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    let prs = vec![consumer(), util_release(1)];
    let refs: Vec<&Pr> = prs.iter().collect();
    let context = context("1.0.0", &refs, limit(1), None);
    let mut queue = ready(prs);
    let mut skipped = Vec::new();
    let found = behind::hold(&context, &feeds, &mut queue, &mut skipped);
    assert_eq!(found.len(), 1);
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].0.key.to_string(), "acme/util#1");
    assert_eq!(skipped.len(), 1);
    assert_eq!(
        skipped[0].reasons,
        vec!["needs a bump of acme.util to 1.3.0"]
    );
    assert_eq!(classify(&skipped[0].reasons), Category::Waiting);
}

fn built(pin: &str, feeds: &Feeds, prs: Vec<Pr>) -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let refs: Vec<&Pr> = prs.iter().collect();
    let context = context(pin, &refs, limit(1), None);
    let manifests = manifests(pin);
    let mut queue = ready(prs);
    let mut skipped = Vec::new();
    let found = behind::hold(&context, feeds, &mut queue, &mut skipped);
    let mut plan = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-11-18T09:00:00-03:00".to_string(),
            train: "deploy/18-11-2026".to_string(),
            deploy_branch: "deploy/18-11-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: queue
                .into_iter()
                .map(|(p, scope)| (p, scope.as_str().to_string()))
                .collect(),
            skipped,
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: publishers::collect(&manifests),
            references: publishers::collect_references(&manifests),
            max_parallel: 4,
            requests: 1,
            rate_remaining: Some(4000),
        },
    );
    plan.bumps = found;
    plan
}

#[test]
fn dependents_of_the_held_pr_wait_for_it() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    let plan = built("1.0.0", &feeds, vec![consumer(), web()]);
    assert!(plan.items.is_empty());
    let web = plan
        .skipped
        .iter()
        .find(|skipped| skipped.id == "acme/web#20")
        .unwrap();
    assert_eq!(web.reasons[0], "depends on acme/sdk#10, which is waiting");
    assert_eq!(classify(&web.reasons), Category::Waiting);
}

#[test]
fn the_plan_json_gains_bumps_only_when_there_are_any() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    let with = serde_json::to_value(built("1.0.0", &feeds, vec![consumer()])).unwrap();
    assert_eq!(with["schema"], 1);
    assert_eq!(with["bumps"][0]["id"], "acme/sdk#10");
    assert_eq!(with["bumps"][0]["package"], "acme.util");
    assert_eq!(with["bumps"][0]["from"], "1.0.0");
    assert_eq!(with["bumps"][0]["to"], "1.3.0");
    let without = serde_json::to_value(built("1.3.0", &feeds, vec![consumer()])).unwrap();
    assert!(without.get("bumps").is_none());
}

#[test]
fn stable_in_major_and_versions_behind_use_the_feed_ordering() {
    let versions: Vec<String> = ["1.9.0", "1.10.0", "1.10.1-rc.1", "2.0.0"]
        .iter()
        .map(|v| (*v).to_string())
        .collect();
    assert_eq!(
        behind::stable_in_major("1.2.0", &versions).as_deref(),
        Some("1.10.0")
    );
    assert_eq!(behind::stable_in_major("1.10.0", &versions), None);
    assert_eq!(behind::versions_behind("1.9.0", &versions), 3);
}

#[test]
fn the_step_detail_names_the_package_versions_repo_and_limit() {
    let feeds = feed(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"]);
    let found = needs("1.0.0", &feeds, limit(1), vec![consumer()]);
    let detail = found[0].step_detail();
    assert!(detail.starts_with("bump acme.util from 1.0.0 to 1.3.0 in acme/sdk"));
    assert!(detail.contains("allows 1 behind"));
}
