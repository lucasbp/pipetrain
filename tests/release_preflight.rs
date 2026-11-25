use std::collections::BTreeMap;

mod common;

use common::pr;
use pipetrain::config::{Config, Rules};
use pipetrain::model::{ChangedFile, Pr};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::planner::release::{self, CODE, Context};
use pipetrain::publishers::{self, Publishers, References};
use pipetrain::workflows::{self, MaxBehind};

type Manifest = (String, String, String);

fn manifest(slug: &str, path: &str, text: &str) -> Manifest {
    (slug.to_string(), path.to_string(), text.to_string())
}

fn manifests() -> Vec<Manifest> {
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

fn touching(mut pr: Pr, path: &str, patch: Option<&str>) -> Pr {
    pr.files = vec![ChangedFile {
        path: path.to_string(),
        patch: patch.map(str::to_string),
    }];
    pr
}

fn util_release(number: u64) -> Pr {
    touching(
        pr("acme/util", number, &format!("feat/u{number}"), "main"),
        "src/Feature.cs",
        None,
    )
}

fn consumer() -> Pr {
    touching(pr("acme/sdk", 10, "feat/s", "main"), "src/Client.cs", None)
}

fn bumping_consumer() -> Pr {
    touching(
        pr("acme/sdk", 10, "feat/s", "main"),
        "src/Acme.Sdk.csproj",
        Some(
            "-<PackageReference Include=\"Acme.Util\" Version=\"1.0.0\" />\n+<PackageReference Include=\"Acme.Util\" Version=\"1.0.3\" />",
        ),
    )
}

fn workflow(limit: &str) -> Vec<(String, String)> {
    vec![(
        ".github/workflows/check.yml".to_string(),
        format!(
            "name: check\njobs:\n  versions:\n    steps:\n      - run: check-versions --max-behind {limit}\n"
        ),
    )]
}

struct Scenario {
    prs: Vec<Pr>,
    limit: Option<MaxBehind>,
    default_max: Option<u32>,
}

fn plan_for(scenario: Scenario) -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let manifests = manifests();
    let publishers: Publishers = publishers::collect(&manifests);
    let references: References = publishers::collect_references(&manifests);
    let refs: Vec<&Pr> = scenario.prs.iter().collect();
    let mut context = Context::new(
        &rules,
        &publishers,
        &references,
        &manifests,
        &refs,
        scenario.default_max,
    );
    if let Some(limit) = scenario.limit {
        context.limits.insert("acme/sdk".to_string(), limit);
    }
    let mut built = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-11-18T09:00:00-03:00".to_string(),
            train: "deploy/18-11-2026".to_string(),
            deploy_branch: "deploy/18-11-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: scenario
                .prs
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
    release::annotate(&mut built, &context);
    built
}

fn three_releases_and(consumer: Pr) -> Vec<Pr> {
    vec![util_release(1), util_release(2), util_release(3), consumer]
}

fn flagged(plan: &Plan) -> Vec<&str> {
    plan.questions
        .iter()
        .filter(|q| q.code == CODE)
        .filter_map(|q| q.pr.as_deref())
        .collect()
}

fn readiness<'a>(plan: &'a Plan, id: &str) -> &'a pipetrain::model::Readiness {
    plan.preflight.iter().find(|r| r.id == id).unwrap()
}

fn found(max: u32) -> Option<MaxBehind> {
    Some(workflows::max_behind(&workflow(&max.to_string())))
}

#[test]
fn consumer_referencing_old_version_is_flagged_when_queue_publishes_more_than_the_limit() {
    let plan = plan_for(Scenario {
        prs: three_releases_and(consumer()),
        limit: found(1),
        default_max: None,
    });
    assert_eq!(flagged(&plan), vec!["acme/sdk#10"]);
    let question = plan.questions.iter().find(|q| q.code == CODE).unwrap();
    assert!(question.message.to_lowercase().contains("acme.util"));
    assert!(question.message.contains("1.0.0"));
    assert!(question.message.contains("3 new versions"));
    assert!(question.message.contains("fails at 1 behind"));
    let ready = readiness(&plan, "acme/sdk#10");
    assert_eq!(ready.status, "waiting");
    assert!(ready.causes.iter().any(|c| c.contains("fails at 1 behind")));
    assert_eq!(plan.schema, 1);
}

#[test]
fn consumer_is_not_flagged_when_the_limit_covers_the_releases() {
    let plan = plan_for(Scenario {
        prs: three_releases_and(consumer()),
        limit: found(4),
        default_max: None,
    });
    assert!(flagged(&plan).is_empty());
    let ready = readiness(&plan, "acme/sdk#10");
    assert!(ready.causes.iter().all(|c| !c.contains("limit")));
    assert!(ready.warnings.is_empty());
}

#[test]
fn unreadable_limit_warns_as_not_confirmed_and_does_not_block() {
    let plan = plan_for(Scenario {
        prs: three_releases_and(consumer()),
        limit: Some(workflows::max_behind(&[])),
        default_max: None,
    });
    assert!(flagged(&plan).is_empty());
    let ready = readiness(&plan, "acme/sdk#10");
    assert!(ready.causes.iter().all(|c| !c.contains("limit")));
    assert!(
        ready.warnings.iter().any(|w| {
            w.starts_with("(not confirmed)") && w.to_lowercase().contains("acme.util")
        })
    );
}

#[test]
fn config_default_applies_when_the_workflows_do_not_say() {
    let plan = plan_for(Scenario {
        prs: three_releases_and(consumer()),
        limit: Some(workflows::max_behind(&[])),
        default_max: Some(1),
    });
    assert_eq!(flagged(&plan), vec!["acme/sdk#10"]);
    let question = plan.questions.iter().find(|q| q.code == CODE).unwrap();
    assert!(question.message.contains("package_max_behind"));
}

#[test]
fn consumer_that_bumps_to_the_published_version_is_not_flagged() {
    let plan = plan_for(Scenario {
        prs: three_releases_and(bumping_consumer()),
        limit: found(1),
        default_max: None,
    });
    assert!(flagged(&plan).is_empty());
    assert!(readiness(&plan, "acme/sdk#10").warnings.is_empty());
}

#[test]
fn releases_count_only_publisher_prs_that_touch_the_package() {
    let docs_only = touching(pr("acme/util", 4, "docs", "main"), "README.md", None);
    let plan = plan_for(Scenario {
        prs: vec![util_release(1), docs_only, consumer()],
        limit: found(2),
        default_max: None,
    });
    assert!(flagged(&plan).is_empty());
}

#[test]
fn annotation_adds_no_edges() {
    let with = plan_for(Scenario {
        prs: three_releases_and(consumer()),
        limit: found(1),
        default_max: None,
    });
    let without = plan_for(Scenario {
        prs: three_releases_and(consumer()),
        limit: found(3),
        default_max: None,
    });
    let edges = |plan: &Plan| plan.items.iter().map(|i| i.after.len()).collect::<Vec<_>>();
    assert_eq!(edges(&with), edges(&without));
    assert_eq!(with.lanes, without.lanes);
}

#[test]
fn max_behind_reads_flags_and_keys() {
    let key = vec![(
        "a.yml".to_string(),
        "jobs:\n  v:\n    with:\n      max-behind: 2\n".to_string(),
    )];
    assert!(matches!(
        workflows::max_behind(&key),
        MaxBehind::Found { max: 2, .. }
    ));
    assert!(matches!(
        workflows::max_behind(&workflow("4")),
        MaxBehind::Found { max: 4, .. }
    ));
}

#[test]
fn max_behind_is_unknown_when_missing_dynamic_or_conflicting() {
    assert!(matches!(
        workflows::max_behind(&[]),
        MaxBehind::Unknown { .. }
    ));
    let dynamic = vec![(
        "a.yml".to_string(),
        "jobs:\n  v:\n    with:\n      max-behind: ${{ vars.LIMIT }}\n".to_string(),
    )];
    assert!(matches!(
        workflows::max_behind(&dynamic),
        MaxBehind::Unknown { .. }
    ));
    let mut conflicting = workflow("1");
    conflicting.push((
        ".github/workflows/other.yml".to_string(),
        "jobs:\n  v:\n    with:\n      max-behind: 3\n".to_string(),
    ));
    assert!(matches!(
        workflows::max_behind(&conflicting),
        MaxBehind::Unknown { .. }
    ));
}
