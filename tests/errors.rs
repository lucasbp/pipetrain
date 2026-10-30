use pipetrain::github::{ApiError, GhProbe, Source, describe_reset, resolve};

const FAKE: &str = "ghp_FAKE0123456789";

fn text(error: ApiError) -> String {
    error.to_string()
}

fn message(text: &str) -> String {
    text.to_string()
}

#[test]
fn reset_without_a_time() {
    assert_eq!(
        describe_reset(100, None),
        "GitHub did not say when it resets"
    );
}

#[test]
fn reset_in_the_past() {
    assert_eq!(
        describe_reset(100, Some(100)),
        "it should already have reset"
    );
    assert_eq!(
        describe_reset(100, Some(40)),
        "it should already have reset"
    );
}

#[test]
fn reset_in_seconds() {
    assert_eq!(describe_reset(100, Some(101)), "it resets in 1 second");
    assert_eq!(describe_reset(100, Some(159)), "it resets in 59 seconds");
}

#[test]
fn reset_in_minutes_rounds_up() {
    assert_eq!(
        describe_reset(100, Some(160)),
        "it resets in about 1 minute"
    );
    assert_eq!(
        describe_reset(100, Some(161)),
        "it resets in about 2 minutes"
    );
    assert_eq!(
        describe_reset(0, Some(3600)),
        "it resets in about 60 minutes"
    );
}

#[test]
fn no_token_names_both_ways_out() {
    let shown = text(ApiError::NoToken);
    assert!(shown.contains("gh auth login"), "{shown}");
    assert!(shown.contains("GH_TOKEN"), "{shown}");
}

#[test]
fn unauthorized_says_what_to_do() {
    let shown = text(ApiError::Unauthorized);
    assert!(shown.contains("401"), "{shown}");
    assert!(shown.contains("gh auth login"), "{shown}");
}

#[test]
fn rate_limited_in_the_past_and_unknown() {
    let past = text(ApiError::RateLimited { reset: Some(1) });
    assert!(past.contains("it should already have reset"), "{past}");
    let unknown = text(ApiError::RateLimited { reset: None });
    assert!(unknown.contains("did not say when"), "{unknown}");
}

#[test]
fn sso_quotes_the_message_and_invents_no_url() {
    let shown = text(ApiError::Sso {
        message: message("Resource protected by organization SAML enforcement"),
    });
    assert!(shown.contains("\"Resource protected by organization SAML enforcement\""));
    assert!(shown.contains("token settings"), "{shown}");
    assert!(!shown.contains("http"), "{shown}");
}

#[test]
fn forbidden_names_the_scopes() {
    let shown = text(ApiError::Forbidden {
        message: message("Resource not accessible"),
    });
    assert!(shown.contains("\"Resource not accessible\""), "{shown}");
    assert!(shown.contains("Pull requests"), "{shown}");
}

#[test]
fn not_found_mentions_private_repos() {
    let shown = text(ApiError::NotFound {
        what: message("acme/api#7"),
    });
    assert!(shown.contains("acme/api#7"), "{shown}");
    assert!(shown.contains("may not see private repos"), "{shown}");
}

#[test]
fn network_points_at_connection_and_proxy() {
    let shown = text(ApiError::Network);
    assert!(shown.contains("cannot reach api.github.com"), "{shown}");
    assert!(shown.contains("proxy"), "{shown}");
}

#[test]
fn graphql_stopped_and_other_keep_their_words() {
    let graphql = text(ApiError::Graphql {
        label: message("search"),
        message: message("boom"),
    });
    assert!(
        graphql.contains("search") && graphql.contains("boom"),
        "{graphql}"
    );
    assert_eq!(
        text(ApiError::Stopped {
            message: message("halt")
        }),
        "halt"
    );
    assert_eq!(
        text(ApiError::Other {
            message: message("odd")
        }),
        "odd"
    );
}

#[test]
fn redaction_scrubs_every_text_field() {
    let leaky = format!("echo {FAKE}");
    let all = [
        ApiError::Sso {
            message: leaky.clone(),
        },
        ApiError::Forbidden {
            message: leaky.clone(),
        },
        ApiError::NotFound {
            what: leaky.clone(),
        },
        ApiError::Graphql {
            label: leaky.clone(),
            message: leaky.clone(),
        },
        ApiError::Stopped {
            message: leaky.clone(),
        },
        ApiError::Other {
            message: leaky.clone(),
        },
    ];
    for error in all {
        let shown = error.redacted(FAKE).to_string();
        assert!(!shown.contains(FAKE), "{shown}");
        assert!(shown.contains("[redacted]"), "{shown}");
    }
}

#[test]
fn redaction_with_an_empty_secret_changes_nothing() {
    let error = ApiError::Other {
        message: message("same"),
    };
    assert_eq!(error.clone().redacted(""), error);
}

#[test]
fn gh_alone_is_the_gh_source() {
    let found = resolve(GhProbe::Token(message(FAKE)), None).unwrap();
    assert_eq!(found.source(), Source::Gh);
    assert_eq!(found.into_token(), FAKE);
}

#[test]
fn env_alone_is_the_env_source() {
    let found = resolve(GhProbe::Missing, Some(format!("  {FAKE}\n"))).unwrap();
    assert_eq!(found.source(), Source::Env);
    assert_eq!(found.into_token(), FAKE);
}

#[test]
fn same_value_from_both_counts_as_env() {
    let found = resolve(GhProbe::Token(message(FAKE)), Some(message(FAKE))).unwrap();
    assert_eq!(found.source(), Source::Env);
}

#[test]
fn different_values_prefer_gh() {
    let found = resolve(GhProbe::Token(message(FAKE)), Some(message("other"))).unwrap();
    assert_eq!(found.source(), Source::Gh);
    assert_eq!(found.into_token(), FAKE);
}

#[test]
fn logged_out_gh_falls_back_to_env() {
    let found = resolve(GhProbe::LoggedOut, Some(message(FAKE))).unwrap();
    assert_eq!(found.source(), Source::Env);
}

#[test]
fn nothing_found_is_no_token() {
    assert_eq!(
        resolve(GhProbe::Missing, None).unwrap_err(),
        ApiError::NoToken
    );
    let blank = resolve(GhProbe::LoggedOut, Some(message("  "))).unwrap_err();
    assert_eq!(blank, ApiError::NoToken);
}

#[test]
fn debug_output_never_holds_the_token() {
    let found = resolve(GhProbe::Token(message(FAKE)), None).unwrap();
    let probe = GhProbe::Token(message(FAKE));
    assert!(!format!("{found:?}").contains(FAKE));
    assert!(!format!("{probe:?}").contains(FAKE));
    assert!(format!("{found:?}").contains("redacted"));
}
