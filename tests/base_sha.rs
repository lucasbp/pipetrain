use pipetrain::github::parse_pr;
use serde_json::{Value, json};

const OID: &str = "0123456789abcdef0123456789abcdef01234567";

fn node(extra: Value) -> Value {
    let mut node = json!({
        "number": 7,
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "headRefOid": "89abcdef0123456789abcdef0123456789abcdef",
    });
    if let (Some(base), Some(more)) = (node.as_object_mut(), extra.as_object()) {
        base.extend(more.clone());
    }
    node
}

#[test]
fn base_sha_comes_from_the_base_ref_target() {
    let pr = parse_pr(&node(json!({ "baseRef": { "target": { "oid": OID } } }))).unwrap();
    assert_eq!(pr.base_sha, OID);
}

#[test]
fn base_sha_is_empty_without_a_base_ref() {
    let pr = parse_pr(&node(json!({}))).unwrap();
    assert_eq!(pr.base_sha, "");
}

#[test]
fn base_sha_is_empty_when_the_base_ref_is_null() {
    let pr = parse_pr(&node(json!({ "baseRef": null }))).unwrap();
    assert_eq!(pr.base_sha, "");
}

#[test]
fn base_sha_is_empty_when_the_target_has_no_oid() {
    let pr = parse_pr(&node(json!({ "baseRef": { "target": null } }))).unwrap();
    assert_eq!(pr.base_sha, "");
}
