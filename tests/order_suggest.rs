mod common;

use common::{Canned, serve};
use pipetrain::command::order::graph::{Analysis, Chain, Edge, analyze};
use pipetrain::command::order::{names_of, suggest};
use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client};
use pipetrain::publishers::Pairs;
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

fn pairs(rows: &[(&str, &str, &str)]) -> Pairs {
    rows.iter()
        .map(|(publisher, consumer, package)| {
            (
                (publisher.to_string(), consumer.to_string()),
                package.to_string(),
            )
        })
        .collect()
}

fn slugs(chain: &Chain) -> Vec<&str> {
    chain.repos.iter().map(String::as_str).collect()
}

#[test]
fn a_component_is_ordered_publisher_first_with_ties_by_slug() {
    let found = analyze(&pairs(&[
        ("org/pub", "org/zed", "pkg.zed"),
        ("org/pub", "org/beta", "pkg.beta"),
        ("x/p", "x/q", "pkg.q"),
    ]));
    assert!(found.cycles.is_empty());
    assert_eq!(found.chains.len(), 2);
    assert_eq!(
        slugs(&found.chains[0]),
        vec!["org/pub", "org/beta", "org/zed"]
    );
    assert_eq!(slugs(&found.chains[1]), vec!["x/p", "x/q"]);
    assert_eq!(
        found.chains[0].edges,
        vec![
            Edge {
                consumer: "org/beta".to_string(),
                package: "pkg.beta".to_string(),
                publisher: "org/pub".to_string(),
            },
            Edge {
                consumer: "org/zed".to_string(),
                package: "pkg.zed".to_string(),
                publisher: "org/pub".to_string(),
            },
        ]
    );
}

#[test]
fn a_repo_without_edges_does_not_appear() {
    let found = analyze(&pairs(&[("org/a", "org/b", "pkg.b")]));
    let all: Vec<&str> = found.chains.iter().flat_map(slugs).collect();
    assert_eq!(all, vec!["org/a", "org/b"]);
}

#[test]
fn a_cycle_has_no_block_and_names_a_pair() {
    let found = analyze(&pairs(&[
        ("org/a", "org/b", "pkg.b"),
        ("org/b", "org/a", "pkg.a"),
        ("x/p", "x/q", "pkg.q"),
    ]));
    assert_eq!(
        found.cycles,
        vec![("org/a".to_string(), "org/b".to_string())]
    );
    assert_eq!(found.chains.len(), 1);
    assert_eq!(slugs(&found.chains[0]), vec!["x/p", "x/q"]);
}

#[test]
fn no_edges_means_no_chains_and_no_cycles() {
    assert_eq!(analyze(&Pairs::new()), Analysis::default());
}

fn canned(body: Value) -> Canned {
    common::canned("200 OK", &body.to_string())
}

fn repo_replies(csproj: &str, text: &str) -> Vec<Canned> {
    vec![
        canned(
            json!({"data": {"repository": {"defaultBranchRef": {"target": {"oid": "abc123"}}}}}),
        ),
        canned(json!({"tree": [
            {"type": "blob", "path": "README.md"},
            {"type": "blob", "path": csproj}
        ], "truncated": false})),
        canned(json!({"data": {"repository": {"f0": {"text": text}}}})),
    ]
}

fn failure(status: &'static str, body: Value) -> Canned {
    common::canned(status, &body.to_string())
}

fn missing_repo() -> Canned {
    canned(json!({"data": {"repository": null}}))
}

fn lenient_client(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

fn sdk_replies() -> Vec<Canned> {
    repo_replies(
        "src/Acme.Sdk.csproj",
        r#"<PackageReference Include="Acme.Util" Version="1.0.0" />"#,
    )
}

fn util_replies() -> Vec<Canned> {
    repo_replies(
        "src/Acme.Util.csproj",
        "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>",
    )
}

#[test]
fn a_repo_that_answers_not_found_is_skipped_and_the_others_still_report() {
    let mut replies = vec![missing_repo()];
    replies.extend(sdk_replies());
    replies.extend(util_replies());
    let (base, server) = serve(replies);
    let mut client = lenient_client(&base);
    let names = names_of([
        "acme/missing".to_string(),
        "acme/sdk".to_string(),
        "acme/util".to_string(),
    ]);

    let found = suggest(&mut client, &names).unwrap();
    server.join().unwrap();

    assert_eq!(
        found.stdout,
        "# acme/sdk references acme.util, published by acme/util\n[[order]]\nrepos = [\"acme/util\", \"acme/sdk\"]\n"
    );
    assert_eq!(
        found.stderr,
        vec![
            "warning: skipped acme/missing: GitHub answered not found, check the name in the config"
                .to_string()
        ]
    );
    assert_eq!(found.exit, 0);
    assert!(!found.stdout.contains("missing"));
}

#[test]
fn every_repo_not_found_is_an_error() {
    let (base, server) = serve(vec![missing_repo(), missing_repo()]);
    let mut client = lenient_client(&base);
    let names = names_of(["acme/one".to_string(), "acme/two".to_string()]);

    let error = suggest(&mut client, &names).unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("no repo could be read"));
}

#[test]
fn an_unauthorized_repo_still_aborts() {
    let (base, server) = serve(vec![failure(
        "401 Unauthorized",
        json!({"message": "Bad credentials"}),
    )]);
    let mut client = lenient_client(&base);
    let names = names_of(["acme/one".to_string(), "acme/two".to_string()]);

    let error = suggest(&mut client, &names).unwrap_err();
    server.join().unwrap();

    assert_eq!(
        error.downcast_ref::<ApiError>(),
        Some(&ApiError::Unauthorized)
    );
    assert!(!error.to_string().contains("no repo could be read"));
}

#[test]
fn a_repo_whose_tree_answers_not_found_is_skipped() {
    let mut replies = vec![
        canned(
            json!({"data": {"repository": {"defaultBranchRef": {"target": {"oid": "abc123"}}}}}),
        ),
        failure("404 Not Found", json!({"message": "Not Found"})),
    ];
    replies.extend(sdk_replies());
    let (base, server) = serve(replies);
    let mut client = lenient_client(&base);
    let names = names_of(["acme/gone".to_string(), "acme/sdk".to_string()]);

    let found = suggest(&mut client, &names).unwrap();
    server.join().unwrap();

    assert!(found.stderr[0].contains("skipped acme/gone"));
    assert!(found.stdout.is_empty());
    assert_eq!(found.exit, 0);
}

#[test]
fn two_repos_over_the_fake_server_print_the_block_and_leak_no_token() {
    let mut replies = repo_replies(
        "src/Acme.Sdk.csproj",
        r#"<PackageReference Include="Acme.Util" Version="1.0.0" />"#,
    );
    replies.extend(repo_replies(
        "src/Acme.Util.csproj",
        "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>",
    ));
    let (base, server) = serve(replies);
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    let mut client = Client::with_base_url(FAKE.to_string(), &cfg, false, &base);
    let names = names_of(["Acme/Sdk".to_string(), "Acme/Util".to_string()]);

    let found = suggest(&mut client, &names).unwrap();
    let seen = server.join().unwrap();

    assert_eq!(
        found.stdout,
        "# Acme/Sdk references acme.util, published by Acme/Util\n[[order]]\nrepos = [\"Acme/Util\", \"Acme/Sdk\"]\n"
    );
    assert!(found.stderr.is_empty());
    assert_eq!(found.exit, 0);
    assert_eq!(seen.len(), 6);
    assert!(
        seen.iter()
            .all(|r| r.to_lowercase().contains("authorization"))
    );
    let printed = format!("{}{}", found.stdout, found.stderr.join("\n")).to_lowercase();
    assert!(!printed.contains("authorization"));
    assert!(!printed.contains(&FAKE.to_lowercase()));
}
