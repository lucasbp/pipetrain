use serde_json::{Value, json};

use super::check::{Check, failure};
use crate::github::{ApiError, Client};
use crate::hint;

const ORG_QUERY: &str = "query($login: String!) { organization(login: $login) { login repositories(privacy: PRIVATE, first: 1) { totalCount } } }";
const USER_QUERY: &str = "query($login: String!) { user(login: $login) { login } }";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Org { private: u64 },
    User,
    Missing,
}

pub fn parse(body: &Value) -> Found {
    if let Some(org) = body.pointer("/data/organization").filter(|v| !v.is_null()) {
        let private = org
            .pointer("/repositories/totalCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Found::Org { private };
    }
    match body.pointer("/data/user") {
        Some(user) if !user.is_null() => Found::User,
        _ => Found::Missing,
    }
}

pub fn probe(client: &mut Client, login: &str) -> Result<Found, ApiError> {
    let variables = json!({ "login": login });
    let body = client
        .query("organization", ORG_QUERY, variables.clone())?
        .into_graphql("organization")?;
    if parse(&body) != Found::Missing {
        return Ok(parse(&body));
    }
    let body = client
        .query("user", USER_QUERY, variables)?
        .into_graphql("user")?;
    Ok(parse(&body))
}

pub fn classify(login: &str, result: &Result<Found, ApiError>) -> Check {
    let id = format!("org:{login}");
    match result {
        Ok(Found::Org { private: 0 }) => Check::warn(
            &id,
            "organization is visible but no private repos came back; if it has private repos, SSO may be hiding them (heuristic)",
            Some(hint::sso_fix()),
        ),
        Ok(Found::Org { private }) => Check::ok(
            &id,
            format!("organization is visible, {private} private repos seen"),
        ),
        Ok(Found::User) => Check::ok(&id, "user account is visible"),
        Ok(Found::Missing) => Check::fail(
            &id,
            "not found, or not visible to this token",
            Some(
                "Check the spelling of the name, and that the token's account can see it."
                    .to_string(),
            ),
        ),
        Err(ApiError::Sso { .. }) => Check::warn(
            &id,
            "GitHub's answer mentions SAML SSO for this org (heuristic)",
            Some(hint::sso_fix()),
        ),
        Err(error) => failure(&id, error),
    }
}

pub fn checks(client: &mut Client, orgs: &[String]) -> Vec<Check> {
    orgs.iter()
        .map(|login| classify(login, &probe(client, login)))
        .collect()
}

pub fn skipped(orgs: &[String]) -> Vec<Check> {
    orgs.iter()
        .map(|login| Check::skipped(&format!("org:{login}"), "needs a working token"))
        .collect()
}
