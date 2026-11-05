use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered};
use pipetrain::publishers::{self, Publishers, References};

type Manifest = (String, String, String);

fn pr(slug: &str, number: u64, head: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        author: String::new(),
        failed_checks: Vec::new(),
        mergeable_unknown: false,
        behind: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

fn bump(mut consumer: Pr, packages: &[&str]) -> Pr {
    let patch: Vec<String> = packages
        .iter()
        .map(|p| format!("-    \"{p}\": \"^1.0.0\",\n+    \"{p}\": \"^1.1.0\","))
        .collect();
    consumer.files = vec![ChangedFile {
        path: "package.json".to_string(),
        patch: Some(patch.join("\n")),
    }];
    consumer
}

fn manifest(slug: &str, path: &str, text: &str) -> Manifest {
    (slug.to_string(), path.to_string(), text.to_string())
}

fn queue(extra: &str, prs: Vec<Pr>, manifests: &[Manifest]) -> Vec<String> {
    emit::queue_lines(&planned(extra, prs, manifests))
}

fn bump_patch(mut consumer: Pr, path: &str, patch: &str) -> Pr {
    consumer.files = vec![ChangedFile {
        path: path.to_string(),
        patch: Some(patch.to_string()),
    }];
    consumer
}

fn planned(extra: &str, prs: Vec<Pr>, manifests: &[Manifest]) -> plan::Plan {
    let cfg = Config::parse(&format!("orgs = [\"acme\"]\n{extra}\n")).unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let publishers: Publishers = publishers::collect(manifests);
    let references: References = publishers::collect_references(manifests);
    plan::build(
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
            requests: 0,
            rate_remaining: None,
        },
    )
}

fn without_waits(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| {
            !l.starts_with("# wait") && !l.starts_with("# step") && !l.starts_with("# lane")
        })
        .cloned()
        .collect()
}

fn waits(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.starts_with("# wait")).collect()
}

fn position(lines: &[String], id: &str) -> usize {
    lines
        .iter()
        .position(|l| l == &format!("merge:{id}"))
        .unwrap()
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

#[test]
fn a_package_edge_puts_a_wait_right_before_the_consumer_merge() {
    let extra = "[packages]\n\"@acme/kit\" = \"acme/kit\"\n";
    let prs = vec![
        bump(pr("acme/web", 5, "feat/a"), &["@acme/kit"]),
        pr("acme/kit", 6, "feat/b"),
    ];
    let lines = queue(extra, prs, &[]);
    let at = position(&lines, "acme/web#5");
    assert_eq!(
        lines[at - 1],
        "# wait: package @acme/kit 1.1.0 from acme/kit#6 must be published before acme/web#5"
    );
    assert!(position(&lines, "acme/kit#6") < at);
    assert_eq!(
        without_waits(&lines),
        vec!["merge:acme/kit#6", "merge:acme/web#5"]
    );
}

#[test]
fn a_dependency_edge_does_not_put_a_wait() {
    let prs = vec![pr("acme/sdk", 1, "feat/a"), pr("acme/util", 2, "feat/b")];
    let lines = queue("", prs, &util_and_sdk());
    assert!(waits(&lines).is_empty());
    assert_eq!(
        without_waits(&lines),
        vec!["merge:acme/util#2", "merge:acme/sdk#1"]
    );
}

#[test]
fn two_publishers_of_one_package_collapse_into_one_wait() {
    let extra = "[packages]\n\"@acme/kit\" = \"acme/kit\"\n";
    let prs = vec![
        bump(pr("acme/web", 5, "feat/a"), &["@acme/kit"]),
        pr("acme/kit", 8, "feat/c"),
        pr("acme/kit", 6, "feat/b"),
    ];
    let lines = queue(extra, prs, &[]);
    let at = position(&lines, "acme/web#5");
    assert_eq!(waits(&lines).len(), 1);
    assert_eq!(
        lines[at - 1],
        "# wait: package @acme/kit 1.1.0 from acme/kit#6 acme/kit#8 must be published before acme/web#5"
    );
    let merges: Vec<&String> = lines.iter().filter(|l| l.starts_with("merge:")).collect();
    assert_eq!(
        merges,
        vec!["merge:acme/kit#6", "merge:acme/kit#8", "merge:acme/web#5"]
    );
}

#[test]
fn without_package_edges_the_queue_has_no_wait() {
    let prs = vec![
        pr("acme/api-core", 1, "feat/a"),
        pr("acme/web", 2, "feat/b"),
    ];
    let lines = queue("", prs, &[]);
    assert_eq!(
        without_waits(&lines),
        vec!["merge:acme/api-core#1", "merge:acme/web#2"]
    );
    assert!(waits(&lines).is_empty());
}

#[test]
fn two_packages_for_one_consumer_come_out_in_a_stable_order() {
    let extra = "[packages]\n\"@acme/zeta\" = \"acme/zeta\"\n\"@acme/alpha\" = \"acme/alpha\"\n";
    let prs = || {
        vec![
            bump(pr("acme/web", 5, "feat/a"), &["@acme/zeta", "@acme/alpha"]),
            pr("acme/zeta", 6, "feat/b"),
            pr("acme/alpha", 7, "feat/c"),
        ]
    };
    let lines = queue(extra, prs(), &[]);
    let at = position(&lines, "acme/web#5");
    assert_eq!(
        lines[at - 2],
        "# wait: package @acme/alpha 1.1.0 from acme/alpha#7 must be published before acme/web#5"
    );
    assert_eq!(
        lines[at - 1],
        "# wait: package @acme/zeta 1.1.0 from acme/zeta#6 must be published before acme/web#5"
    );
    assert_eq!(waits(&lines).len(), 2);
    assert_eq!(lines, queue(extra, prs(), &[]));
}

const CMUTIL_EXTRA: &str = "[packages]\n\"cmutil\" = \"acme/cmutil\"\n";

fn cmutil_prs(version: &str) -> Vec<Pr> {
    let patch = format!(
        "-    <PackageReference Include=\"CmUtil\" Version=\"1.2610.3022.39\" />\n+    <PackageReference Include=\"CmUtil\" Version=\"{version}\" />"
    );
    vec![
        bump_patch(pr("acme/api", 5, "feat/a"), "src/Api/Api.csproj", &patch),
        pr("acme/cmutil", 6, "feat/b"),
    ]
}

fn consumer_after(built: &plan::Plan) -> serde_json::Value {
    let items = serde_json::to_value(&built.items).unwrap();
    let consumer = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "acme/api#5")
        .unwrap();
    consumer["after"][0].clone()
}

#[test]
fn the_wait_names_the_version_the_consumer_bumps_to() {
    let lines = queue(CMUTIL_EXTRA, cmutil_prs("1.2610.3022.40"), &[]);
    let at = position(&lines, "acme/api#5");
    assert_eq!(
        lines[at - 1],
        "# wait: package cmutil 1.2610.3022.40 from acme/cmutil#6 must be published before acme/api#5"
    );
}

#[test]
fn an_unreadable_version_keeps_the_old_wait_format() {
    let lines = queue(CMUTIL_EXTRA, cmutil_prs("$(CmUtilVersion)"), &[]);
    let at = position(&lines, "acme/api#5");
    assert_eq!(
        lines[at - 1],
        "# wait: package cmutil from acme/cmutil#6 must be published before acme/api#5"
    );
}

#[test]
fn json_after_carries_the_version_only_when_known() {
    let known = planned(CMUTIL_EXTRA, cmutil_prs("1.2610.3022.40"), &[]);
    let entry = consumer_after(&known);
    assert_eq!(entry["package"], "cmutil");
    assert_eq!(entry["version"], "1.2610.3022.40");
    let unknown = planned(CMUTIL_EXTRA, cmutil_prs("$(CmUtilVersion)"), &[]);
    let entry = consumer_after(&unknown);
    assert_eq!(entry["package"], "cmutil");
    assert!(entry.get("version").is_none());
}
