use std::collections::BTreeSet;

use pipetrain::conflicts::{Snapshot, find, kind_of};

fn snap(id: &str, repo: &str, paths: &[&str]) -> Snapshot {
    Snapshot {
        id: id.to_string(),
        repo: repo.to_string(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
    }
}

fn none() -> BTreeSet<(String, String)> {
    BTreeSet::new()
}

#[test]
fn kinds_follow_the_path() {
    let table = [
        ("package-lock.json", "lockfile"),
        ("web/Yarn.lock", "lockfile"),
        ("apps/site/pnpm-lock.yaml", "lockfile"),
        ("src/Api/packages.lock.json", "lockfile"),
        ("db/schema.sql", "sql"),
        ("services/api/db/migrations/001.sql", "sql"),
        ("migrations/002.sql", "sql"),
        ("db/notes.md", "other"),
        ("scripts/seed.sql", "other"),
        ("src/Api/obj/project.assets.json", "generated"),
        ("bin/Debug/app.dll", "generated"),
        ("src/main.rs", "other"),
        ("README.md", "other"),
    ];
    for (path, kind) in table {
        assert_eq!(kind_of(path), kind, "{path}");
    }
}

#[test]
fn two_prs_of_one_repo_sharing_files_make_one_conflict_with_sorted_files() {
    let found = find(
        &[
            snap(
                "acme/api#2",
                "acme/api",
                &["c.rs", "a.rs", "b.rs", "only2.rs"],
            ),
            snap(
                "acme/api#1",
                "acme/api",
                &["b.rs", "c.rs", "a.rs", "only1.rs"],
            ),
        ],
        &none(),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].a, "acme/api#1");
    assert_eq!(found[0].b, "acme/api#2");
    assert_eq!(found[0].repo, "acme/api");
    assert_eq!(found[0].files, vec!["a.rs", "b.rs", "c.rs"]);
    assert_eq!(found[0].kind, "other");
    assert_eq!(found[0].file_count(), "3 files");
}

#[test]
fn one_shared_file_reads_singular() {
    let found = find(
        &[
            snap("acme/api#1", "acme/api", &["package-lock.json"]),
            snap("acme/api#2", "acme/api", &["package-lock.json"]),
        ],
        &none(),
    );
    assert_eq!(found[0].kind, "lockfile");
    assert_eq!(found[0].file_count(), "1 file");
}

#[test]
fn different_repos_do_not_conflict() {
    let found = find(
        &[
            snap("acme/api#1", "acme/api", &["a.rs"]),
            snap("acme/web#2", "acme/web", &["a.rs"]),
        ],
        &none(),
    );
    assert!(found.is_empty());
}

#[test]
fn a_stacked_pair_is_left_out() {
    let stacked: BTreeSet<(String, String)> = [
        ("acme/api#1".to_string(), "acme/api#2".to_string()),
        ("acme/api#2".to_string(), "acme/api#1".to_string()),
    ]
    .into_iter()
    .collect();
    let found = find(
        &[
            snap("acme/api#1", "acme/api", &["a.rs"]),
            snap("acme/api#2", "acme/api", &["a.rs"]),
        ],
        &stacked,
    );
    assert!(found.is_empty());
}

#[test]
fn mixed_kinds_collapse_to_other() {
    let found = find(
        &[
            snap(
                "acme/api#1",
                "acme/api",
                &["db/001.sql", "package-lock.json"],
            ),
            snap(
                "acme/api#2",
                "acme/api",
                &["db/001.sql", "package-lock.json"],
            ),
        ],
        &none(),
    );
    assert_eq!(found[0].kind, "other");
}

#[test]
fn three_prs_sharing_a_file_make_three_conflicts_in_order() {
    let found = find(
        &[
            snap("acme/api#3", "acme/api", &["a.rs"]),
            snap("acme/api#1", "acme/api", &["a.rs"]),
            snap("acme/api#2", "acme/api", &["a.rs"]),
        ],
        &none(),
    );
    let pairs: Vec<(&str, &str)> = found.iter().map(|c| (c.a.as_str(), c.b.as_str())).collect();
    assert_eq!(
        pairs,
        vec![
            ("acme/api#1", "acme/api#2"),
            ("acme/api#1", "acme/api#3"),
            ("acme/api#2", "acme/api#3"),
        ]
    );
}
