use anyhow::{Context, Result, ensure};
use serde_json::Value;
use ureq::http::Uri;

fn endpoint(text: &str) -> Option<Uri> {
    if text.contains(['?', '#', '%', '\\'])
        || text.split('/').any(|part| matches!(part, "." | ".."))
    {
        return None;
    }
    let uri: Uri = text.strip_suffix('/').unwrap_or(text).parse().ok()?;
    (matches!(uri.scheme_str(), Some("http" | "https")) && !uri.authority()?.as_str().contains('@'))
        .then_some(uri)
}

fn logged_endpoint(destination: &str) -> Option<Uri> {
    let value = match destination.chars().next()? {
        quote @ ('\'' | '"') => destination
            .strip_suffix("...")
            .unwrap_or(destination)
            .strip_prefix(quote)?
            .strip_suffix(quote)?,
        _ => destination,
    };
    endpoint(value)
}

fn publishes(resource: &Value) -> bool {
    let kind = &resource["@type"];
    kind == "PackagePublish/2.0.0"
        || kind
            .as_array()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "PackagePublish/2.0.0"))
}

pub(super) fn check(url: &str, index: &Value, org: &str, destinations: &[&str]) -> Result<()> {
    let index_url = endpoint(url).context("invalid publisher service index URL")?;
    let resources = index["resources"]
        .as_array()
        .context("missing NuGet service resources")?;
    let mut publish = None;
    for resource in resources.iter().filter(|resource| publishes(resource)) {
        let uri = resource["@id"]
            .as_str()
            .and_then(endpoint)
            .context("invalid NuGet PackagePublish endpoint")?;
        ensure!(
            uri.scheme() == index_url.scheme() && uri.authority() == index_url.authority(),
            "NuGet PackagePublish is outside the publisher service origin"
        );
        if index_url
            .host()
            .is_some_and(|host| host.eq_ignore_ascii_case("nuget.pkg.github.com"))
        {
            ensure!(
                uri.path()
                    .split('/')
                    .nth(1)
                    .is_some_and(|owner| owner.eq_ignore_ascii_case(org)),
                "NuGet PackagePublish is outside the publisher organization"
            );
        }
        ensure!(
            publish.as_ref().is_none_or(|known| known == &uri),
            "ambiguous NuGet PackagePublish endpoints"
        );
        publish = Some(uri);
    }
    let publish =
        publish.context("publisher service index has no NuGet PackagePublish endpoint")?;
    ensure!(
        !destinations.is_empty()
            && destinations
                .iter()
                .all(|destination| logged_endpoint(destination)
                    .is_some_and(|uri| uri == index_url || uri == publish)),
        "NuGet push destination is missing or does not match the publisher feed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication::{PublisherExecution, PublisherRun, verify_pushes};
    use serde_json::json;

    const INDEX: &str = "https://nuget.pkg.github.com/acme/index.json";
    const PUBLISH: &str = "https://nuget.pkg.github.com/acme/";

    fn index() -> Value {
        json!({"resources": [{"@type": "PackagePublish/2.0.0", "@id": PUBLISH}]})
    }

    #[test]
    fn a_version_already_in_the_feed_does_not_prove_a_push_to_it() {
        let execution = PublisherExecution {
            repository: "acme/lib".into(),
            merge_sha: "a".repeat(40),
            run_id: 12,
            published_this_execution: true,
        };
        for (second, valid) in [
            ("'https://nuget.pkg.github.com/acme'...", true),
            (INDEX, true),
            ("'https://nuget.pkg.github.com/other/'...", false),
            ("'https://api.nuget.org/acme/'...", false),
            ("'https://nuget.pkg.github.com/acme/?x=1'...", false),
            (
                "'https://nuget.pkg.github.com/acme'... or another feed",
                false,
            ),
            ("", false),
        ] {
            let log = [PUBLISH, second]
                .map(|destination| {
                    format!(
                        "Pushing Acme.Lib.1.1.0.nupkg to {destination}\nYour package was pushed."
                    )
                })
                .join("\n");
            let run = PublisherRun {
                repository: execution.repository.clone(),
                head_sha: execution.merge_sha.clone(),
                id: execution.run_id,
                succeeded: true,
                push_step_succeeded: true,
                push_step_log: log,
            };
            let result = verify_pushes(&execution, &run, "Acme.Lib", |version, destinations| {
                check(INDEX, &index(), "acme", destinations)?;
                Ok(version == "1.1.0")
            });
            assert_eq!(result.is_ok(), valid, "{second}");
        }
    }

    #[test]
    fn invalid_or_ambiguous_index_resources_cannot_authorize_a_destination() {
        for wrong in [
            "https://other.example/acme",
            "https://nuget.pkg.github.com/other",
            "https://nuget.pkg.github.com/acme/another",
        ] {
            let mut resources = index();
            resources["resources"]
                .as_array_mut()
                .unwrap()
                .push(json!({"@type":["PackagePublish/2.0.0"],"@id":wrong}));
            assert!(
                check(INDEX, &resources, "acme", &[INDEX]).is_err(),
                "{wrong}"
            );
        }
        assert!(check(INDEX, &json!({"resources":[]}), "acme", &[INDEX]).is_err());
    }
}
