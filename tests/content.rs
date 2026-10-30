use pipetrain::content::{self, Check, MAX_FILES, MAX_LOOKUPS, Probe};
use pipetrain::discovery::Scope;
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use serde_json::{Value, json};

const TRAIN: &str = "deploy/29-10-2026";

fn pr(slug: &str, number: u64, head: &str, base: &str, paths: &[&str]) -> Pr {
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
        files: paths
            .iter()
            .map(|path| ChangedFile {
                path: path.to_string(),
                patch: None,
            })
            .collect(),
        files_complete: true,
    }
}

fn check(slug: &str, reference: &str, sha: &str, paths: &[&str]) -> Check {
    Check {
        slug: slug.to_string(),
        reference: reference.to_string(),
        sha: sha.to_string(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
    }
}

fn oid(text: &str) -> Value {
    json!({ "object": { "oid": text } })
}

fn absent() -> Value {
    json!({ "object": null })
}

fn response(items: Vec<Value>) -> Value {
    let mut data = serde_json::Map::new();
    for (i, item) in items.into_iter().enumerate() {
        data.insert(format!("p{i}"), item);
    }
    json!({ "data": data })
}

fn readings(items: Vec<Value>) -> Vec<content::Reading> {
    let count = items.len();
    content::parse(&response(items), count)
}

#[test]
fn probes_start_with_the_ref_guard_then_pair_base_and_head_per_path() {
    let checks = vec![check(
        "acme/api",
        "master",
        "aaaa1111",
        &["src/a.rs", "docs/b.md"],
    )];
    let expressions: Vec<String> = content::probes(&checks)
        .into_iter()
        .map(|p| p.expression)
        .collect();
    assert_eq!(
        expressions,
        vec![
            "master",
            "master:src/a.rs",
            "aaaa1111:src/a.rs",
            "master:docs/b.md",
            "aaaa1111:docs/b.md",
        ]
    );
}

#[test]
fn query_has_one_alias_per_probe_and_all_values_are_variables() {
    let probes = vec![
        Probe {
            slug: "acme/api".to_string(),
            expression: "master:src/a.rs".to_string(),
        },
        Probe {
            slug: "acme/web".to_string(),
            expression: "bbbb2222:src/\"quoted\" path.rs".to_string(),
        },
    ];
    let (query, variables) = content::build_query(&probes);
    assert!(query.starts_with("query($o0: String!, $r0: String!, $e0: String!, $o1"));
    assert!(
        query.contains("p0: repository(owner: $o0, name: $r0) { object(expression: $e0) { oid } }")
    );
    assert!(
        query.contains("p1: repository(owner: $o1, name: $r1) { object(expression: $e1) { oid } }")
    );
    assert!(!query.contains("acme"));
    assert!(!query.contains("master"));
    assert!(!query.contains("quoted"));
    assert!(!query.contains("mutation"));
    assert_eq!(variables["o0"], "acme");
    assert_eq!(variables["r0"], "api");
    assert_eq!(variables["e0"], "master:src/a.rs");
    assert_eq!(variables["o1"], "acme");
    assert_eq!(variables["r1"], "web");
    assert_eq!(variables["e1"], "bbbb2222:src/\"quoted\" path.rs");
}

#[test]
fn a_hundred_file_check_needs_more_than_one_chunk_of_fifty_lookups() {
    let paths: Vec<String> = (0..MAX_FILES).map(|i| format!("src/f{i}.rs")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let probes = content::probes(&[check("acme/api", "master", "aaaa1111", &refs)]);
    assert_eq!(probes.len(), 1 + 2 * MAX_FILES);
    let sizes: Vec<usize> = probes.chunks(MAX_LOOKUPS).map(<[Probe]>::len).collect();
    assert_eq!(sizes, vec![50, 50, 50, 50, 1]);
    for chunk in probes.chunks(MAX_LOOKUPS) {
        let (query, variables) = content::build_query(chunk);
        assert!(query.contains("p49:") || chunk.len() < 50);
        assert!(!query.contains(&format!("p{}:", chunk.len())));
        assert_eq!(variables.as_object().unwrap().len(), chunk.len() * 3);
    }
}

#[test]
fn parse_distinguishes_oid_absent_and_unreadable() {
    let parsed = readings(vec![
        oid("abc"),
        absent(),
        Value::Null,
        json!({ "object": { "oid": 7 } }),
    ]);
    assert_eq!(parsed[0], Some(Some("abc".to_string())));
    assert_eq!(parsed[1], Some(None));
    assert_eq!(parsed[2], None);
    assert_eq!(parsed[3], None);
}

#[test]
fn parse_without_data_is_all_unreadable() {
    assert_eq!(content::parse(&json!({}), 2), vec![None, None]);
}

#[test]
fn all_equal_is_the_same_content() {
    let checks = vec![check("acme/api", "master", "aaaa1111", &["a.rs", "b.rs"])];
    let parsed = readings(vec![oid("commit"), oid("x"), oid("x"), oid("y"), oid("y")]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![true]);
}

#[test]
fn one_different_file_is_not_the_same_content() {
    let checks = vec![check("acme/api", "master", "aaaa1111", &["a.rs", "b.rs"])];
    let parsed = readings(vec![oid("commit"), oid("x"), oid("x"), oid("y"), oid("z")]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![false]);
}

#[test]
fn null_against_null_counts_as_an_already_applied_deletion() {
    let checks = vec![check("acme/api", "master", "aaaa1111", &["gone.rs"])];
    let parsed = readings(vec![oid("commit"), absent(), absent()]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![true]);
}

#[test]
fn null_against_an_oid_is_different() {
    let checks = vec![check(
        "acme/api",
        "master",
        "aaaa1111",
        &["new.rs", "old.rs"],
    )];
    let added = readings(vec![oid("commit"), absent(), oid("x"), oid("y"), oid("y")]);
    assert_eq!(content::verdicts(&checks, &added), vec![false]);
    let deleted = readings(vec![oid("commit"), oid("x"), absent(), oid("y"), oid("y")]);
    assert_eq!(content::verdicts(&checks, &deleted), vec![false]);
}

#[test]
fn an_unreadable_file_keeps_the_pr() {
    let checks = vec![check("acme/api", "master", "aaaa1111", &["a.rs", "b.rs"])];
    let parsed = readings(vec![
        oid("commit"),
        oid("x"),
        oid("x"),
        Value::Null,
        oid("y"),
    ]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![false]);
}

#[test]
fn a_missing_ref_keeps_the_pr_even_when_every_path_is_null() {
    let checks = vec![check("acme/api", "deploy/x", "aaaa1111", &["gone.rs"])];
    let parsed = readings(vec![absent(), absent(), absent()]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![false]);
}

#[test]
fn a_truncated_response_keeps_the_pr() {
    let checks = vec![check("acme/api", "master", "aaaa1111", &["a.rs", "b.rs"])];
    let parsed = readings(vec![oid("commit"), oid("x"), oid("x")]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![false]);
}

#[test]
fn verdicts_stay_aligned_across_checks_of_different_widths() {
    let checks = vec![
        check("acme/api", "master", "aaaa1111", &["a.rs"]),
        check("acme/web", "master", "bbbb2222", &["a.rs", "b.rs"]),
        check("acme/web", "master", "cccc3333", &["c.rs"]),
    ];
    let parsed = readings(vec![
        oid("c"),
        oid("x"),
        oid("x"),
        oid("c"),
        oid("x"),
        oid("x"),
        oid("y"),
        oid("w"),
        oid("c"),
        oid("z"),
        oid("z"),
    ]);
    assert_eq!(content::verdicts(&checks, &parsed), vec![true, false, true]);
}

#[test]
fn checks_cover_base_and_train_for_a_normal_pr() {
    let ready = vec![(
        pr("acme/api", 5, "feature", "master", &["a.rs", "b.rs"]),
        Scope::Train,
    )];
    let checks = content::checks(&ready, TRAIN);
    let refs: Vec<&str> = checks.iter().map(|c| c.reference.as_str()).collect();
    assert_eq!(refs, vec!["master", TRAIN]);
    assert_eq!(checks[0].paths, vec!["a.rs", "b.rs"]);
    assert_eq!(checks[0].sha, format!("{:040x}", 5));
}

#[test]
fn checks_skip_prs_that_are_not_eligible() {
    let mut incomplete = pr("acme/api", 1, "f1", "master", &["a.rs"]);
    incomplete.files_complete = false;
    let empty = pr("acme/api", 2, "f2", "master", &[]);
    let many: Vec<String> = (0..=MAX_FILES).map(|i| format!("f{i}.rs")).collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let big = pr("acme/api", 3, "f3", "master", &many_refs);
    let mut no_sha = pr("acme/api", 4, "f4", "master", &["a.rs"]);
    no_sha.head_sha = String::new();
    let ready = vec![
        (incomplete, Scope::Train),
        (empty, Scope::Train),
        (big, Scope::Train),
        (no_sha, Scope::Train),
    ];
    assert!(content::checks(&ready, TRAIN).is_empty());
}

#[test]
fn exactly_one_hundred_files_is_still_checked() {
    let exact: Vec<String> = (0..MAX_FILES).map(|i| format!("f{i}.rs")).collect();
    let refs: Vec<&str> = exact.iter().map(String::as_str).collect();
    let ready = vec![(pr("acme/api", 1, "f1", "master", &refs), Scope::Train)];
    assert_eq!(content::checks(&ready, TRAIN).len(), 2);
}

fn run(ready: Vec<(Pr, Scope)>, items: Vec<Value>) -> (Vec<Pr>, Vec<(Pr, Vec<String>)>) {
    let checks = content::checks(&ready, TRAIN);
    let parsed = readings(items);
    let verdicts = content::verdicts(&checks, &parsed);
    let (kept, skipped) = content::apply(ready, TRAIN, &verdicts);
    (
        kept.into_iter().map(|(p, _)| p).collect(),
        skipped.into_iter().map(|s| (s.pr, s.reasons)).collect(),
    )
}

fn identical(count: usize) -> Vec<Value> {
    let mut items = vec![oid("commit")];
    for i in 0..count {
        let hash = format!("blob{i}");
        items.push(oid(&hash));
        items.push(oid(&hash));
    }
    items
}

fn differing_second_file() -> Vec<Value> {
    vec![oid("commit"), oid("a"), oid("a"), oid("b"), oid("c")]
}

#[test]
fn equal_content_in_the_base_is_skipped_with_the_same_content_reason() {
    let ready = vec![(
        pr("acme/api", 7, "feature", "master", &["a.rs", "b.rs"]),
        Scope::Train,
    )];
    let mut items = identical(2);
    items.extend(vec![
        oid("commit"),
        oid("a"),
        oid("zzz"),
        oid("b"),
        oid("b"),
    ]);
    let (kept, skipped) = run(ready, items);
    assert!(kept.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0.key.number, 7);
    assert_eq!(skipped[0].1, vec!["already in master (same content)"]);
}

#[test]
fn equal_content_only_in_the_train_branch_names_the_train() {
    let ready = vec![(
        pr("acme/api", 8, "feature", "master", &["a.rs"]),
        Scope::Train,
    )];
    let items = vec![
        oid("commit"),
        oid("a"),
        oid("zzz"),
        oid("commit"),
        oid("a"),
        oid("a"),
    ];
    let (kept, skipped) = run(ready, items);
    assert!(kept.is_empty());
    assert_eq!(
        skipped[0].1,
        vec![format!("already in {TRAIN} (same content)")]
    );
}

#[test]
fn one_differing_file_keeps_the_pr() {
    let ready = vec![(
        pr("acme/api", 9, "feature", "master", &["a.rs", "b.rs"]),
        Scope::Train,
    )];
    let mut items = differing_second_file();
    items.extend(differing_second_file());
    let (kept, skipped) = run(ready, items);
    assert_eq!(kept.len(), 1);
    assert!(skipped.is_empty());
}

#[test]
fn incomplete_file_lists_are_kept_and_never_queried() {
    let mut incomplete = pr("acme/api", 10, "feature", "master", &["a.rs"]);
    incomplete.files_complete = false;
    let ready = vec![(incomplete, Scope::Train)];
    assert!(content::checks(&ready, TRAIN).is_empty());
    let (kept, skipped) = content::apply(ready, TRAIN, &[]);
    assert_eq!(kept.len(), 1);
    assert!(skipped.is_empty());
}

#[test]
fn verdicts_line_up_with_prs_when_ineligible_ones_sit_in_between() {
    let mut incomplete = pr("acme/api", 11, "f11", "master", &["a.rs"]);
    incomplete.files_complete = false;
    let ready = vec![
        (pr("acme/api", 12, "f12", "master", &["a.rs"]), Scope::Train),
        (incomplete, Scope::Train),
        (pr("acme/web", 13, "f13", "master", &["a.rs"]), Scope::Train),
    ];
    let mut items = vec![
        oid("commit"),
        oid("a"),
        oid("a"),
        oid("commit"),
        oid("a"),
        oid("zzz"),
    ];
    items.extend(vec![
        oid("commit"),
        oid("a"),
        oid("b"),
        oid("commit"),
        oid("a"),
        oid("b"),
    ]);
    let (kept, skipped) = run(ready, items);
    let kept_numbers: Vec<u64> = kept.iter().map(|p| p.key.number).collect();
    assert_eq!(kept_numbers, vec![11, 13]);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0.key.number, 12);
}

#[test]
fn a_pr_into_the_train_is_only_checked_against_its_own_base() {
    let ready = vec![(
        pr("acme/api", 14, "feature", TRAIN, &["a.rs"]),
        Scope::IntoTrain,
    )];
    let checks = content::checks(&ready, TRAIN);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].reference, TRAIN);
}
