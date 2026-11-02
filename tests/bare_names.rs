mod common;

use common::{Canned, serve};
use pipetrain::command::names::{self, NameError, Target};
use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client};
use pipetrain::model::PrKey;

const FAKE: &str = "ghp_FAKE0123456789";

fn found(owner: &str, name: &str) -> Canned {
    common::canned(
        "200 OK",
        &format!(r#"{{"name":"{name}","owner":{{"login":"{owner}"}}}}"#),
    )
}

fn missing() -> Canned {
    common::canned("404 Not Found", r#"{"message":"Not Found"}"#)
}

fn client(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

fn orgs(list: &[&str]) -> Vec<String> {
    list.iter().map(|org| org.to_string()).collect()
}

fn bare(repo: &str, number: u64) -> Target {
    Target::Bare {
        repo: repo.to_string(),
        number,
    }
}

fn request_line(request: &str) -> &str {
    request.lines().next().unwrap()
}

fn refs(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|text| text.to_string()).collect()
}

#[test]
fn name_resolves_to_the_only_org_that_has_it() {
    let (base, server) = serve(vec![missing(), found("globex", "api")]);
    let mut client = client(&base);
    let keys = names::resolve(
        &mut client,
        vec![bare("api", 12)],
        &orgs(&["acme", "globex"]),
    )
    .unwrap();
    let seen = server.join().unwrap();
    assert_eq!(keys, vec![PrKey::new("globex", "api", 12)]);
    assert!(request_line(&seen[0]).starts_with("GET /repos/acme/api HTTP/1.1"));
    assert!(request_line(&seen[1]).starts_with("GET /repos/globex/api HTTP/1.1"));
    assert_eq!(client.requests(), 2);
}

#[test]
fn github_spelling_wins_over_the_typed_name() {
    let (base, server) = serve(vec![found("acme", "Api")]);
    let keys = names::resolve(&mut client(&base), vec![bare("api", 1)], &orgs(&["acme"])).unwrap();
    server.join().unwrap();
    assert_eq!(keys, vec![PrKey::new("acme", "Api", 1)]);
}

#[test]
fn full_reference_makes_no_request() {
    let (base, server) = serve(vec![]);
    let mut client = client(&base);
    let key = PrKey::new("acme", "web", 2);
    let keys = names::resolve(
        &mut client,
        vec![Target::Full(key.clone())],
        &orgs(&["acme", "globex"]),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(keys, vec![key]);
    assert_eq!(client.requests(), 0);
}

#[test]
fn name_in_two_orgs_is_ambiguous_and_never_chosen() {
    let (base, server) = serve(vec![found("acme", "api"), found("globex", "api")]);
    let error = names::resolve(
        &mut client(&base),
        vec![bare("api", 1)],
        &orgs(&["acme", "globex"]),
    )
    .unwrap_err();
    server.join().unwrap();
    let error = error.downcast::<NameError>().unwrap();
    assert_eq!(
        error,
        NameError::Ambiguous {
            reference: "api#1".to_string(),
            candidates: vec!["acme/api".to_string(), "globex/api".to_string()],
        }
    );
    let message = error.to_string();
    assert!(message.contains("acme/api, globex/api"), "{message}");
    assert!(message.contains("org/api#1"), "{message}");
}

#[test]
fn name_in_no_org_is_not_found_with_the_next_step() {
    let (base, server) = serve(vec![missing(), missing()]);
    let error = names::resolve(
        &mut client(&base),
        vec![bare("ghost", 1)],
        &orgs(&["acme", "globex"]),
    )
    .unwrap_err();
    server.join().unwrap();
    let error = error.downcast::<NameError>().unwrap();
    assert_eq!(
        error,
        NameError::NotFound {
            reference: "ghost#1".to_string(),
            orgs: orgs(&["acme", "globex"]),
        }
    );
    let message = error.to_string();
    assert!(message.contains("acme, globex"), "{message}");
    assert!(message.contains("org/ghost#1"), "{message}");
    assert!(message.contains("--org"), "{message}");
}

#[test]
fn no_orgs_is_an_error_without_a_request() {
    let (base, server) = serve(vec![]);
    let mut client = client(&base);
    let error = names::resolve(&mut client, vec![bare("api", 1)], &[]).unwrap_err();
    server.join().unwrap();
    let error = error.downcast::<NameError>().unwrap();
    assert_eq!(
        error,
        NameError::NoOrgs {
            reference: "api#1".to_string()
        }
    );
    assert_eq!(client.requests(), 0);
}

#[test]
fn repeated_name_is_looked_up_once() {
    let (base, server) = serve(vec![found("acme", "api")]);
    let mut client = client(&base);
    let targets = vec![bare("api", 1), bare("API", 2), bare("api", 3)];
    let keys = names::resolve(&mut client, targets, &orgs(&["acme"])).unwrap();
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(client.requests(), 1);
    assert_eq!(
        keys,
        vec![
            PrKey::new("acme", "api", 1),
            PrKey::new("acme", "api", 2),
            PrKey::new("acme", "api", 3),
        ]
    );
}

#[test]
fn distinct_names_are_looked_up_separately() {
    let (base, server) = serve(vec![found("acme", "api"), found("acme", "web")]);
    let mut client = client(&base);
    let targets = vec![bare("api", 1), bare("web", 2)];
    let keys = names::resolve(&mut client, targets, &orgs(&["acme"])).unwrap();
    server.join().unwrap();
    assert_eq!(client.requests(), 2);
    assert_eq!(
        keys,
        vec![PrKey::new("acme", "api", 1), PrKey::new("acme", "web", 2)]
    );
}

#[test]
fn unauthorized_is_not_mistaken_for_missing() {
    let reply = common::canned("401 Unauthorized", r#"{"message":"Bad credentials"}"#);
    let (base, server) = serve(vec![reply]);
    let error =
        names::resolve(&mut client(&base), vec![bare("api", 1)], &orgs(&["acme"])).unwrap_err();
    server.join().unwrap();
    assert_eq!(
        error.downcast::<ApiError>().unwrap(),
        ApiError::Unauthorized
    );
}

#[test]
fn parse_targets_accepts_full_urls_and_bare_names() {
    let targets = names::parse_targets(&refs(&[
        "acme/api#1",
        "https://github.com/acme/web/pull/2",
        "api#3",
    ]))
    .unwrap();
    assert_eq!(
        targets,
        vec![
            Target::Full(PrKey::new("acme", "api", 1)),
            Target::Full(PrKey::new("acme", "web", 2)),
            bare("api", 3),
        ]
    );
    assert!(names::has_bare(&targets));
    assert!(!names::has_bare(&targets[..2]));
}

#[test]
fn parse_targets_rejects_malformed_references() {
    for raw in ["api", "#12", "api#x", "a/b/c#1"] {
        let error = names::parse_targets(&refs(&[raw])).unwrap_err().to_string();
        assert!(error.contains(raw), "{error}");
        assert!(error.contains("repo#123"), "{error}");
    }
}
