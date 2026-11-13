use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::detect::{bumped_packages, bumped_versions};
use pipetrain::emit;
use pipetrain::manifest::{published, references};
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered};
use pipetrain::publishers;

type Manifest = (String, String, String);

fn names(found: std::collections::BTreeSet<String>) -> Vec<String> {
    found.into_iter().collect()
}

fn diff(path: &str, patch: &str) -> Vec<ChangedFile> {
    vec![ChangedFile {
        path: path.to_string(),
        patch: Some(patch.to_string()),
    }]
}

fn version_of(path: &str, patch: &str, package: &str) -> Option<String> {
    bumped_versions(&diff(path, patch)).get(package).cloned()
}

fn bumped(path: &str, patch: &str, package: &str) -> bool {
    bumped_packages(&diff(path, patch)).contains(package)
}

#[test]
fn cargo_publishes_the_package_name() {
    assert_eq!(
        published(
            "crates/util/Cargo.toml",
            "[package]\nname = \"Acme_Util\"\n"
        )
        .as_deref(),
        Some("acme-util")
    );
    assert_eq!(
        published("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
        None
    );
    assert_eq!(
        published(
            "Cargo.toml",
            "[package]\nname = \"acme\"\npublish = false\n"
        ),
        None
    );
}

#[test]
fn cargo_references_cover_dependency_tables() {
    let text = "[dependencies]\nacme-util = \"1\"\n[dev-dependencies]\nacme_test = { path = \"../t\" }\n[workspace.dependencies]\nacme-core = { version = \"2\" }\n";
    assert_eq!(
        names(references("Cargo.toml", text)),
        vec!["acme-core", "acme-test", "acme-util"]
    );
    assert!(references("Cargo.toml", "[package").is_empty());
}

#[test]
fn cargo_versions_are_read_only_when_readable() {
    let path = "Cargo.toml";
    let inline = "-acme-util = \"1.0.0\"\n+acme-util = { version = \"1.1.0\", features = [\"x\"] }";
    assert_eq!(
        version_of(path, inline, "acme-util").as_deref(),
        Some("1.1.0")
    );
    let plain = " [dependencies]\n-acme-util = \"1.0\"\n+acme-util = \"^1.1\"";
    assert_eq!(version_of(path, plain, "acme-util").as_deref(), Some("1.1"));
    let table = " [dependencies.acme-util]\n-version = \"1.0.0\"\n+version = \"1.1.0\"";
    assert_eq!(
        version_of(path, table, "acme-util").as_deref(),
        Some("1.1.0")
    );
    for local in [
        "-acme-util = \"1.0.0\"\n+acme-util = { path = \"../util\" }",
        "-acme-util = \"1.0.0\"\n+acme-util = { workspace = true }",
        "-acme-util = \"1.0.0\"\n+acme-util = { git = \"https://example.com/u\", version = \"1.1.0\" }",
        "-acme-util = \"1.0.0\"\n+acme-util = { path = \"../u\", version = \"1.1.0\" }",
    ] {
        assert!(!bumped(path, local, "acme-util"), "{local}");
    }
    let range = "-acme-util = \"1.0.0\"\n+acme-util = \">=1.1, <2\"";
    assert!(bumped(path, range, "acme-util"));
    assert_eq!(version_of(path, range, "acme-util"), None);
    let header_git = " [dependencies.acme-util]\n git = \"https://example.com/u\"\n-version = \"1.0.0\"\n+version = \"1.1.0\"";
    assert!(!bumped(path, header_git, "acme-util"));
}

#[test]
fn cargo_package_settings_are_not_dependencies() {
    let patch =
        "-version = \"1.0.0\"\n+version = \"1.1.0\"\n-edition = \"2021\"\n+edition = \"2024\"";
    assert!(bumped_packages(&diff("Cargo.toml", patch)).is_empty());
    let own = " [package]\n-version = \"1.0.0\"\n+version = \"1.1.0\"";
    assert!(bumped_packages(&diff("Cargo.toml", own)).is_empty());
}

#[test]
fn pyproject_publishes_project_or_poetry_name() {
    assert_eq!(
        published("pyproject.toml", "[project]\nname = \"Acme.Util_Core\"\n").as_deref(),
        Some("acme-util-core")
    );
    assert_eq!(
        published("lib/pyproject.toml", "[tool.poetry]\nname = \"acme-lib\"\n").as_deref(),
        Some("acme-lib")
    );
    assert_eq!(
        published(
            "pyproject.toml",
            "[tool.poetry]\nname = \"acme\"\npackage-mode = false\n"
        ),
        None
    );
    assert_eq!(
        published(
            "pyproject.toml",
            "[project]\nname = \"acme\"\nclassifiers = [\"Private :: Do Not Upload\"]\n"
        ),
        None
    );
}

#[test]
fn pyproject_references_cover_project_groups_and_poetry() {
    let text = r#"[project]
name = "acme-app"
dependencies = ["acme-util>=1.0", "Acme_Core[extra]==2.0 ; python_version > '3.9'"]

[project.optional-dependencies]
dev = ["acme-test"]

[dependency-groups]
lint = ["acme-lint~=1.2", {include-group = "dev"}]

[tool.poetry.dependencies]
python = "^3.11"
acme-poetry = "^1.0"

[tool.poetry.group.dev.dependencies]
acme-dev = { version = "^2", extras = ["x"] }
"#;
    assert_eq!(
        names(references("pyproject.toml", text)),
        vec![
            "acme-core",
            "acme-dev",
            "acme-lint",
            "acme-poetry",
            "acme-test",
            "acme-util"
        ]
    );
}

#[test]
fn pyproject_versions_keep_one_bound_and_drop_the_rest() {
    let path = "pyproject.toml";
    let pep = "-    \"acme-util>=1.0.0\",\n+    \"acme-util>=1.1.0\",";
    assert_eq!(version_of(path, pep, "acme-util").as_deref(), Some("1.1.0"));
    let exact = "-    \"acme-util==1.0.0\",\n+    \"Acme_Util==1.1.0\",";
    assert_eq!(
        version_of(path, exact, "acme-util").as_deref(),
        Some("1.1.0")
    );
    let compatible = "-    \"acme-util~=1.0\",\n+    \"acme-util~=1.1\",";
    assert_eq!(
        version_of(path, compatible, "acme-util").as_deref(),
        Some("1.1")
    );
    let inline = "-dependencies = [\"acme-util>=1.0\"]\n+dependencies = [\"acme-util>=1.1\"]";
    assert_eq!(
        version_of(path, inline, "acme-util").as_deref(),
        Some("1.1")
    );
    for unreadable in [
        "-    \"acme-util>=1.0\",\n+    \"acme-util>=1.1,<2\",",
        "-    \"acme-util>=1.0\",\n+    \"acme-util<2\",",
        "-    \"acme-util>=1.0\",\n+    \"acme-util==1.*\",",
    ] {
        assert!(bumped(path, unreadable, "acme-util"), "{unreadable}");
        assert_eq!(
            version_of(path, unreadable, "acme-util"),
            None,
            "{unreadable}"
        );
    }
    let git = "-    \"acme-util>=1.0\",\n+    \"acme-util @ git+https://example.com/u\",";
    assert!(!bumped(path, git, "acme-util"));
}

#[test]
fn poetry_versions_follow_the_same_rules() {
    let path = "pyproject.toml";
    let plain = " [tool.poetry.dependencies]\n-acme-util = \"^1.0\"\n+acme-util = \"^1.1\"";
    assert_eq!(version_of(path, plain, "acme-util").as_deref(), Some("1.1"));
    let inline = "-acme-util = \"^1.0\"\n+acme-util = { version = \"^1.1\", extras = [\"x\"] }";
    assert_eq!(
        version_of(path, inline, "acme-util").as_deref(),
        Some("1.1")
    );
    for local in [
        "-acme-util = \"^1.0\"\n+acme-util = { path = \"../util\", develop = true }",
        "-acme-util = \"^1.0\"\n+acme-util = { git = \"https://example.com/u\" }",
    ] {
        assert!(!bumped(path, local, "acme-util"), "{local}");
    }
    let table = " [tool.poetry.dependencies.acme-util]\n-version = \"^1.0\"\n+version = \"^1.1\"";
    assert_eq!(version_of(path, table, "acme-util").as_deref(), Some("1.1"));
    let range = "-acme-util = \"^1.0\"\n+acme-util = \">=1.1,<2\"";
    assert!(bumped(path, range, "acme-util"));
    assert_eq!(version_of(path, range, "acme-util"), None);
    let settings = " [project]\n-version = \"1.0.0\"\n+version = \"1.1.0\"";
    assert!(bumped_packages(&diff(path, settings)).is_empty());
}

#[test]
fn go_mod_publishes_the_module_path() {
    assert_eq!(
        published("go.mod", "module github.com/Acme/Util/v2\n\ngo 1.22\n").as_deref(),
        Some("github.com/acme/util/v2")
    );
    assert_eq!(published("go.mod", "go 1.22\n"), None);
}

#[test]
fn go_mod_references_cover_blocks_and_skip_replace() {
    let text = "module github.com/acme/app\n\ngo 1.22\n\nrequire github.com/acme/one v1.0.0\n\nrequire (\n\tgithub.com/acme/two v1.2.3 // indirect\n)\n\nreplace github.com/acme/three => ../three\n";
    assert_eq!(
        names(references("go.mod", text)),
        vec!["github.com/acme/one", "github.com/acme/two"]
    );
}

#[test]
fn go_mod_versions_skip_pseudo_versions_and_replace() {
    let path = "go.mod";
    let block = " require (\n-\tgithub.com/acme/util v1.0.0\n+\tgithub.com/acme/util v1.1.0\n )";
    assert_eq!(
        version_of(path, block, "github.com/acme/util").as_deref(),
        Some("1.1.0")
    );
    let single =
        "-require github.com/acme/util v1.0.0\n+require github.com/acme/util v2.0.0+incompatible";
    assert_eq!(
        version_of(path, single, "github.com/acme/util").as_deref(),
        Some("2.0.0")
    );
    let pseudo = "-\tgithub.com/acme/util v1.0.0\n+\tgithub.com/acme/util v0.0.0-20260101120000-abcdef123456";
    assert!(bumped(path, pseudo, "github.com/acme/util"));
    assert_eq!(version_of(path, pseudo, "github.com/acme/util"), None);
    let replace = " replace (\n-\tgithub.com/acme/util v1.0.0 => ../util\n+\tgithub.com/acme/util v1.1.0 => ../util\n )";
    assert!(!bumped(path, replace, "github.com/acme/util"));
    let directive = "-go 1.21\n+go 1.22\n-toolchain go1.21.0\n+toolchain go1.22.0";
    assert!(bumped_packages(&diff(path, directive)).is_empty());
}

fn pr(slug: &str, number: u64) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: "feature".to_string(),
        default_branch: String::new(),
        merge_state_status: String::new(),
        check_details: Vec::new(),
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

fn queue(prs: Vec<Pr>, manifests: &[Manifest]) -> Vec<String> {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
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
            publishers: publishers::collect(manifests),
            references: publishers::collect_references(manifests),
            max_parallel: 4,
            requests: 0,
            rate_remaining: None,
        },
    );
    emit::queue_lines(&built)
}

fn manifest(slug: &str, path: &str, text: &str) -> Manifest {
    (slug.to_string(), path.to_string(), text.to_string())
}

fn chain(
    lib_path: &str,
    lib_text: &str,
    app_path: &str,
    app_text: &str,
    patch: Option<&str>,
) -> Vec<String> {
    let lib = pr("acme/lib", 1);
    let mut app = pr("acme/app", 2);
    if let Some(patch) = patch {
        app.files = diff(app_path, patch);
    }
    queue(
        vec![app, lib],
        &[
            manifest("acme/lib", lib_path, lib_text),
            manifest("acme/app", app_path, app_text),
        ],
    )
}

fn merges(lines: &[String]) -> Vec<&str> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("merge:"))
        .collect()
}

fn waits(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.starts_with("# wait")).collect()
}

#[test]
fn cargo_dependency_orders_the_publisher_first_and_a_bump_waits() {
    let lib = "[package]\nname = \"acme-lib\"\n";
    let app = "[package]\nname = \"acme-app\"\n[dependencies]\nacme-lib = \"1.0.0\"\n";
    let plain = chain("Cargo.toml", lib, "Cargo.toml", app, None);
    assert_eq!(merges(&plain), vec!["acme/lib#1", "acme/app#2"]);
    assert!(waits(&plain).is_empty());
    let patch = "-acme-lib = \"1.0.0\"\n+acme-lib = \"1.1.0\"";
    let bumped = chain("Cargo.toml", lib, "Cargo.toml", app, Some(patch));
    assert_eq!(merges(&bumped), vec!["acme/lib#1", "acme/app#2"]);
    assert_eq!(
        waits(&bumped),
        vec!["# wait: package acme-lib 1.1.0 from acme/lib#1 must be published before acme/app#2"]
    );
    let local = "-acme-lib = \"1.0.0\"\n+acme-lib = { path = \"../lib\" }";
    assert!(waits(&chain("Cargo.toml", lib, "Cargo.toml", app, Some(local))).is_empty());
}

#[test]
fn pyproject_dependency_orders_the_publisher_first_and_a_bump_waits() {
    let lib = "[project]\nname = \"acme-lib\"\n";
    let app = "[project]\nname = \"acme-app\"\ndependencies = [\n    \"acme-lib>=1.0.0\",\n]\n";
    let plain = chain("pyproject.toml", lib, "pyproject.toml", app, None);
    assert_eq!(merges(&plain), vec!["acme/lib#1", "acme/app#2"]);
    let patch = "-    \"acme-lib>=1.0.0\",\n+    \"acme-lib>=1.1.0\",";
    let bumped = chain("pyproject.toml", lib, "pyproject.toml", app, Some(patch));
    assert_eq!(
        waits(&bumped),
        vec!["# wait: package acme-lib 1.1.0 from acme/lib#1 must be published before acme/app#2"]
    );
    let range = "-    \"acme-lib>=1.0.0\",\n+    \"acme-lib>=1.1.0,<2\",";
    let unreadable = chain("pyproject.toml", lib, "pyproject.toml", app, Some(range));
    assert_eq!(
        waits(&unreadable),
        vec!["# wait: package acme-lib from acme/lib#1 must be published before acme/app#2"]
    );
}

#[test]
fn go_dependency_orders_the_publisher_first_and_a_bump_waits() {
    let lib = "module github.com/acme/lib\n\ngo 1.22\n";
    let app = "module github.com/acme/app\n\ngo 1.22\n\nrequire github.com/acme/lib v1.0.0\n";
    let plain = chain("go.mod", lib, "go.mod", app, None);
    assert_eq!(merges(&plain), vec!["acme/lib#1", "acme/app#2"]);
    let patch = "-require github.com/acme/lib v1.0.0\n+require github.com/acme/lib v1.1.0";
    let bumped = chain("go.mod", lib, "go.mod", app, Some(patch));
    assert_eq!(
        waits(&bumped),
        vec![
            "# wait: package github.com/acme/lib 1.1.0 from acme/lib#1 must be published before acme/app#2"
        ]
    );
}

#[test]
fn ecosystems_do_not_link_to_each_other() {
    let lib = "[package]\nname = \"acme-lib\"\n";
    let app = "[project]\nname = \"acme-app\"\ndependencies = [\"other-lib>=1\"]\n";
    let lines = chain("Cargo.toml", lib, "pyproject.toml", app, None);
    assert!(waits(&lines).is_empty());
}
