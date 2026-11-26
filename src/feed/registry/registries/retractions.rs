use super::*;

fn release(raw: &str) -> Result<semver::Version> {
    let raw = raw.trim().trim_matches(['"', '`']);
    let Some(version) = raw.strip_prefix('v') else {
        anyhow::bail!("invalid Go retraction version")
    };
    Ok(semver::Version::parse(version)?)
}

fn ranges(text: &str, package: &str) -> Result<Vec<(semver::Version, semver::Version)>> {
    let mut block = false;
    let mut found = Vec::new();
    let mut module = None;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("module")
            .filter(|v| v.starts_with(char::is_whitespace))
        {
            module = Some(rest.trim().trim_matches(['"', '`']));
        }
        let spec = if block {
            if line == ")" {
                block = false;
                continue;
            }
            line
        } else {
            let Some(spec) = line
                .strip_prefix("retract")
                .filter(|s| s.starts_with(char::is_whitespace) || s.starts_with('('))
            else {
                continue;
            };
            let spec = spec.trim();
            if spec == "(" {
                block = true;
                continue;
            }
            spec
        };
        let (from, to) =
            if let Some(interval) = spec.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let (from, to) = interval
                    .split_once(',')
                    .context("invalid Go retraction interval")?;
                (release(from)?, release(to)?)
            } else {
                let v = release(spec)?;
                (v.clone(), v)
            };
        ensure!(
            !from.cmp_precedence(&to).is_gt(),
            "invalid Go retraction interval"
        );
        found.push((from, to));
    }
    ensure!(
        module == Some(package) && !block,
        "invalid go.mod from module proxy"
    );
    Ok(found)
}

pub(crate) fn exclude_go_retractions(
    session: &Session,
    package: &str,
    fetched: Fetched,
) -> Result<Fetched> {
    let Fetched::Found { feed, mut versions } = fetched else {
        return Ok(fetched);
    };
    let highest = versions.iter().max_by(|a, b| {
        let go = Ecosystem::Go;
        go.is_prerelease(b)
            .cmp(&go.is_prerelease(a))
            .then_with(|| go.compare_versions(a, b))
    });
    if let Some(version) = highest {
        let module = go_path(package)?;
        let url = format!("{feed}/{module}/@v/{}.mod", go_escape(version));
        let excluded = match session.get_body(Ecosystem::Go, &url, |body| {
            ranges(&body.read_to_string()?, package)
        })? {
            Outcome::Body(ranges) => ranges,
            Outcome::NotFound => anyhow::bail!(
                "cannot select a Go upgrade without the publisher's retractions; check the module version and pass --to"
            ),
            Outcome::Refused { had_token } => return Ok(Fetched::Refused { had_token }),
        };
        versions.retain(|text| {
            let Ok(v) = release(text) else { return false };
            !excluded
                .iter()
                .any(|(from, to)| !v.cmp_precedence(from).is_lt() && !v.cmp_precedence(to).is_gt())
        });
    }
    Ok(Fetched::Found { feed, versions })
}
