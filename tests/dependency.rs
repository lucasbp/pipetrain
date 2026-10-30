use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered};
use pipetrain::planner::{self, PlanInput, Planned};
use pipetrain::publishers::{self, Publishers, References};

type Manifest = (String, String, String);

fn pr(slug: &str, number: u64, head: &str, base: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
        base_ref: base.to_string(),
        head_sha: format!("{number:040x}"),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
    }
}

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

fn queue(cfg: &Config, prs: Vec<Pr>, manifests: &[Manifest]) -> Vec<String> {
    let rules = Rules::new(cfg).unwrap();
    let (publishers, references) = scan(manifests);
    let built = plan::build(
        &rules,
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
        },
    );
    emit::queue_lines(&built)
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
    assert_eq!(dep.reason, "sdk references acme.util, published by util");
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
