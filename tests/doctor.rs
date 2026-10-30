use std::path::PathBuf;

use pipetrain::doctor::check::{self, Check, ConfigState, LOW_RATE, Status};
use pipetrain::doctor::orgs::{self, Found};
use pipetrain::doctor::report;
use pipetrain::github::{ApiError, GhProbe, resolve};
use pipetrain::hint::{self, GhState, Os};
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

fn lookup() -> Result<pipetrain::github::Lookup, ApiError> {
    resolve(GhProbe::Token(FAKE.to_string()), None)
}

fn viewer(login: &str) -> Result<Value, ApiError> {
    Ok(json!({ "data": { "viewer": { "login": login } } }))
}

fn rate_body(remaining: u64) -> Result<Value, ApiError> {
    Ok(
        json!({ "resources": { "core": { "remaining": remaining, "limit": 5000, "reset": 1_000 } } }),
    )
}

#[test]
fn token_found_is_ok_and_names_the_source() {
    let c = check::token(&lookup(), GhState::LoggedIn, Os::Linux);
    assert_eq!(c.id, "token");
    assert_eq!(c.status, Status::Ok);
    assert!(c.message.contains("gh auth token"));
    assert_eq!(c.fix, None);
}

#[test]
fn token_from_env_names_gh_token() {
    let found = resolve(GhProbe::Missing, Some(FAKE.to_string()));
    let c = check::token(&found, GhState::Missing, Os::Linux);
    assert_eq!(c.status, Status::Ok);
    assert!(c.message.contains("GH_TOKEN"));
}

#[test]
fn token_missing_fails_with_the_hint_for_the_os() {
    let found = resolve(GhProbe::Missing, None);
    let c = check::token(&found, GhState::Missing, Os::Windows);
    assert_eq!(c.status, Status::Fail);
    assert_eq!(
        c.fix,
        Some(hint::missing_token_fix(Os::Windows, GhState::Missing))
    );
    assert_eq!(
        c.message,
        hint::short_no_token(Os::Windows, GhState::Missing)
    );
}

#[test]
fn api_ok_names_the_viewer() {
    let c = check::api(&viewer("octocat"));
    assert_eq!(c.status, Status::Ok);
    assert!(c.message.contains("octocat"));
}

#[test]
fn api_without_a_viewer_login_fails() {
    let c = check::api(&Ok(json!({ "data": {} })));
    assert_eq!(c.status, Status::Fail);
}

#[test]
fn api_401_fails_with_the_renew_hint() {
    let c = check::api(&Err(ApiError::Unauthorized));
    assert_eq!(c.status, Status::Fail);
    assert!(c.message.contains("401"));
    assert_eq!(c.fix, Some(hint::renew_token_fix()));
}

#[test]
fn api_network_error_fails_with_the_connectivity_hint() {
    let c = check::api(&Err(ApiError::Network));
    assert_eq!(c.status, Status::Fail);
    assert!(c.message.contains("cannot reach"));
    assert_eq!(c.fix, Some(hint::connectivity_fix()));
}

#[test]
fn rate_with_room_is_ok() {
    let c = check::rate(&rate_body(4_000), 0);
    assert_eq!(c.status, Status::Ok);
    assert!(c.message.contains("4000 of 5000"));
}

#[test]
fn rate_under_the_threshold_warns() {
    let c = check::rate(&rate_body(LOW_RATE - 1), 0);
    assert_eq!(c.status, Status::Warn);
    assert!(c.fix.is_some());
}

#[test]
fn rate_at_the_threshold_is_ok() {
    assert_eq!(check::rate(&rate_body(LOW_RATE), 0).status, Status::Ok);
}

#[test]
fn rate_without_core_numbers_fails() {
    let c = check::rate(&Ok(json!({})), 0);
    assert_eq!(c.status, Status::Fail);
}

#[test]
fn rate_error_is_classified_like_any_other() {
    let c = check::rate(&Err(ApiError::Unauthorized), 0);
    assert_eq!(c.id, "rate");
    assert_eq!(c.status, Status::Fail);
}

#[test]
fn config_found_is_ok_and_shows_the_path() {
    let c = check::config(&ConfigState::Found(PathBuf::from("pipetrain.toml")));
    assert_eq!(c.status, Status::Ok);
    assert!(c.message.contains("pipetrain.toml"));
}

#[test]
fn config_not_found_warns_with_a_fix() {
    let c = check::config(&ConfigState::NotFound);
    assert_eq!(c.status, Status::Warn);
    assert!(c.message.contains("no config file found"));
    assert!(c.fix.is_some());
}

#[test]
fn config_invalid_fails_and_keeps_the_parser_message() {
    let c = check::config(&ConfigState::Invalid("expected `=` at line 3".to_string()));
    assert_eq!(c.status, Status::Fail);
    assert!(c.message.contains("line 3"));
}

#[test]
fn org_with_private_repos_is_ok() {
    let c = orgs::classify("acme", &Ok(Found::Org { private: 12 }));
    assert_eq!(c.id, "org:acme");
    assert_eq!(c.status, Status::Ok);
    assert!(c.message.contains("12 private repos"));
}

#[test]
fn org_without_private_repos_warns_about_sso_as_a_heuristic() {
    let c = orgs::classify("acme", &Ok(Found::Org { private: 0 }));
    assert_eq!(c.status, Status::Warn);
    assert!(c.message.contains("heuristic"));
    assert_eq!(c.fix, Some(hint::sso_fix()));
}

#[test]
fn user_account_is_ok() {
    let c = orgs::classify("octocat", &Ok(Found::User));
    assert_eq!(c.status, Status::Ok);
}

#[test]
fn org_not_visible_fails() {
    let c = orgs::classify("ghost", &Ok(Found::Missing));
    assert_eq!(c.status, Status::Fail);
    assert!(c.message.contains("not visible"));
}

#[test]
fn org_sso_message_warns_with_the_sso_fix() {
    let error = ApiError::Sso {
        message: "Resource protected by organization SAML enforcement".to_string(),
    };
    let c = orgs::classify("acme", &Err(error));
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix, Some(hint::sso_fix()));
}

#[test]
fn org_network_error_fails() {
    let c = orgs::classify("acme", &Err(ApiError::Network));
    assert_eq!(c.id, "org:acme");
    assert_eq!(c.status, Status::Fail);
}

#[test]
fn parse_tells_org_user_and_missing_apart() {
    let org = json!({ "data": { "organization": { "login": "a", "repositories": { "totalCount": 3 } } } });
    let user = json!({ "data": { "organization": null, "user": { "login": "u" } } });
    let none = json!({ "data": { "organization": null, "user": null } });
    assert_eq!(orgs::parse(&org), Found::Org { private: 3 });
    assert_eq!(orgs::parse(&user), Found::User);
    assert_eq!(orgs::parse(&none), Found::Missing);
}

#[test]
fn skipped_checks_warn_and_carry_the_reason() {
    let list = orgs::skipped(&["a".to_string(), "b".to_string()]);
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|c| c.status == Status::Warn));
    assert!(list[0].message.contains("needs a working token"));
}

fn sample() -> Vec<Check> {
    vec![
        Check::ok("token", "found a token from `gh auth token`"),
        Check::warn(
            "config",
            "no config file found",
            Some("Create it.".to_string()),
        ),
        Check::fail(
            "api",
            "GitHub rejected the token (401)",
            Some("Renew it.".to_string()),
        ),
    ]
}

#[test]
fn json_has_the_documented_shape() {
    let value: Value = serde_json::from_str(&report::json(&sample())).unwrap();
    assert_eq!(value["schema"], 1);
    let checks = value["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3);
    for entry in checks {
        let keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["fix", "id", "message", "status"]);
    }
    assert_eq!(checks[0]["status"], "ok");
    assert_eq!(checks[0]["fix"], Value::Null);
    assert_eq!(checks[1]["status"], "warn");
    assert_eq!(checks[2]["fix"], "Renew it.");
}

#[test]
fn json_has_no_ansi_codes() {
    assert!(!report::json(&sample()).contains('\u{1b}'));
}

fn plain(text: &str) -> String {
    anstream::adapter::strip_str(text).to_string()
}

#[test]
fn text_lists_each_check_and_a_summary() {
    let text = plain(&report::text(&sample()));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("[ ok ] token found a token from `gh auth token`"));
    assert!(text.contains("[warn] config no config file found"));
    assert!(text.contains("[fail] api GitHub rejected the token (401)"));
    assert!(text.contains("       Renew it."));
    assert!(text.ends_with("1 ok, 1 warn, 1 fail\n"));
}

#[test]
fn text_hides_the_fix_of_a_passing_check() {
    let ok = Check {
        fix: Some("hidden".to_string()),
        ..Check::ok("token", "fine")
    };
    assert!(!plain(&report::text(&[ok])).contains("hidden"));
}

#[test]
fn exit_code_is_one_only_when_something_fails() {
    assert_eq!(report::exit_code(&sample()), 1);
    let fine = vec![Check::ok("token", "x"), Check::warn("config", "y", None)];
    assert_eq!(report::exit_code(&fine), 0);
    assert_eq!(report::exit_code(&[]), 0);
}

#[test]
fn the_token_never_appears_in_any_output() {
    let found = lookup();
    let mut checks = vec![check::token(&found, GhState::LoggedIn, Os::Linux)];
    checks.push(check::api(&viewer("octocat")));
    checks.push(check::api(&Err(ApiError::Unauthorized)));
    checks.push(check::rate(&rate_body(1), 0));
    checks.push(orgs::classify("acme", &Ok(Found::Org { private: 0 })));
    assert!(!report::json(&checks).contains(FAKE));
    assert!(!report::text(&checks).contains(FAKE));
    assert!(!format!("{checks:?}").contains(FAKE));
    assert!(!format!("{found:?}").contains(FAKE));
}
