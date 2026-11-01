use std::path::{Path, PathBuf};

fn all(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.chars().all(allowed)
}

fn valid_name(name: &str) -> bool {
    let chars =
        |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-');
    all(name, chars) && name.chars().any(|c| c != '.')
}

fn valid_endpoint(endpoint: &str) -> bool {
    all(endpoint, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
    })
}

fn valid_part(part: &str) -> bool {
    all(part, |c| c.is_ascii_alphanumeric())
}

fn split_slug(slug: &str) -> Option<(String, String)> {
    let (org, repo) = slug
        .to_ascii_lowercase()
        .split_once('/')
        .map(|(o, r)| (o.to_owned(), r.to_owned()))?;
    (valid_name(&org) && valid_name(&repo)).then_some((org, repo))
}

pub(super) fn entry_path(
    dir: &Path,
    slug: &str,
    endpoint: &str,
    parts: &[&str],
) -> Option<PathBuf> {
    let (org, repo) = split_slug(slug)?;
    if !valid_endpoint(endpoint) || !parts.iter().all(|p| valid_part(p)) {
        return None;
    }
    let mut file = endpoint.to_owned();
    for part in parts {
        file.push('-');
        file.push_str(part);
    }
    file.push_str(".json");
    Some(dir.join(org).join(repo).join(file))
}
