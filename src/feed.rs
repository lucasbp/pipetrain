use std::cmp::Ordering;
use std::time::Duration;

use clap::ValueEnum;
use serde_json::{Value, json};

pub const NPM_PUBLIC: &str = "https://registry.npmjs.org";
pub const NPM_GITHUB: &str = "https://npm.pkg.github.com";
pub const NUGET_GITHUB: &str = "https://nuget.pkg.github.com";
pub const RECENT: usize = 5;
pub const EXIT_NOT_PUBLISHED: i32 = 1;
pub const EXIT_REFUSED: i32 = 2;
pub const EXIT_ERROR: i32 = 3;
pub const DEFAULT_INTERVAL: u64 = 10;
pub const DEFAULT_TIMEOUT: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Ecosystem {
    Nuget,
    Npm,
}

impl Ecosystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nuget => "nuget",
            Self::Npm => "npm",
        }
    }

    pub fn default_for(package: &str) -> Self {
        if package.starts_with('@') {
            Self::Npm
        } else {
            Self::Nuget
        }
    }
}

pub fn npm_base(package: &str, org: Option<&str>, registry: Option<&str>) -> String {
    if let Some(registry) = registry {
        return registry.trim_end_matches('/').to_string();
    }
    if package.starts_with('@') && org.is_some() {
        NPM_GITHUB.to_string()
    } else {
        NPM_PUBLIC.to_string()
    }
}

pub fn npm_url(base: &str, package: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        package.replace('/', "%2f")
    )
}

pub fn nuget_index_url(org: Option<&str>, registry: Option<&str>) -> Option<String> {
    match (registry, org) {
        (Some(registry), _) => Some(registry.to_string()),
        (None, Some(org)) => Some(format!("{NUGET_GITHUB}/{org}/index.json")),
        (None, None) => None,
    }
}

pub fn nuget_versions_url(base: &str, package: &str) -> String {
    format!(
        "{}/{}/index.json",
        base.trim_end_matches('/'),
        package.to_lowercase()
    )
}

pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    authority.split(':').next().unwrap_or_default()
}

pub fn is_github_packages(url: &str) -> bool {
    let host = host(url).to_ascii_lowercase();
    host == "pkg.github.com" || host.ends_with(".pkg.github.com")
}

pub fn npm_versions(body: &Value) -> Vec<String> {
    body.get("versions")
        .and_then(Value::as_object)
        .map(|versions| versions.keys().cloned().collect())
        .unwrap_or_default()
}

pub fn nuget_base_address(index: &Value) -> Option<String> {
    index
        .get("resources")?
        .as_array()?
        .iter()
        .find(|resource| {
            resource
                .get("@type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("PackageBaseAddress/"))
        })?
        .get("@id")?
        .as_str()
        .map(str::to_string)
}

pub fn nuget_versions(body: &Value) -> Vec<String> {
    body.get("versions")
        .and_then(Value::as_array)
        .map(|versions| {
            versions
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Number(u64),
    Text(String),
}

impl Ord for Part {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.cmp(b),
            (Self::Number(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Number(_)) => Ordering::Greater,
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Part {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    numbers: Vec<u64>,
    pre: Vec<Part>,
}

impl Version {
    pub fn numbers(&self) -> &[u64] {
        &self.numbers
    }

    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split('+').next().unwrap_or_default();
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, pre),
            None => (text, ""),
        };
        let mut numbers: Vec<u64> = core
            .split('.')
            .map(|segment| segment.parse().unwrap_or(0))
            .collect();
        while numbers.len() > 1 && numbers.last() == Some(&0) {
            numbers.pop();
        }
        let pre = pre
            .split('.')
            .filter(|segment| !segment.is_empty())
            .map(|segment| match segment.parse() {
                Ok(number) => Part::Number(number),
                Err(_) => Part::Text(segment.to_ascii_lowercase()),
            })
            .collect();
        Self { numbers, pre }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers.cmp(&other.numbers).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn compare(a: &str, b: &str) -> Ordering {
    Version::parse(a).cmp(&Version::parse(b))
}

pub fn newest_first(versions: &[String]) -> Vec<String> {
    let mut sorted = versions.to_vec();
    sorted.sort_by(|a, b| compare(b, a).then_with(|| b.cmp(a)));
    sorted.dedup_by(|a, b| compare(a, b) == Ordering::Equal);
    sorted
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub package: String,
    pub ecosystem: Ecosystem,
    pub feed: String,
    pub version: Option<String>,
    pub published: Option<bool>,
    pub latest: Option<String>,
    pub versions: Vec<String>,
}

impl Report {
    pub fn build(
        package: &str,
        ecosystem: Ecosystem,
        feed: &str,
        version: Option<&str>,
        found: &[String],
    ) -> Self {
        let ordered = newest_first(found);
        let published = version.map(|wanted| {
            ordered
                .iter()
                .any(|have| compare(have, wanted) == Ordering::Equal)
        });
        Self {
            package: package.to_string(),
            ecosystem,
            feed: feed.to_string(),
            version: version.map(str::to_string),
            published,
            latest: ordered.first().cloned(),
            versions: ordered.into_iter().take(RECENT).collect(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.published == Some(false) {
            EXIT_NOT_PUBLISHED
        } else {
            0
        }
    }
}

pub fn render_text(report: &Report) -> String {
    let package = &report.package;
    match (&report.version, report.published) {
        (Some(version), Some(true)) => format!("{package} {version}: published"),
        (Some(version), _) => match &report.latest {
            Some(latest) => format!("{package} {version}: not published (latest {latest})"),
            None => format!("{package} {version}: not published (no versions)"),
        },
        (None, _) => match &report.latest {
            Some(latest) => format!(
                "{package}: latest {latest}\nlast {}: {}",
                report.versions.len(),
                report.versions.join(", ")
            ),
            None => format!("{package}: no versions published"),
        },
    }
}

pub fn render_json(report: &Report) -> Value {
    json!({
        "schema": 1,
        "package": report.package,
        "ecosystem": report.ecosystem.as_str(),
        "feed": report.feed,
        "version": report.version,
        "published": report.published,
        "latest": report.latest,
        "versions": report.versions,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPlan {
    pub interval: Duration,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    Published(Report),
    Pending(Report),
    Failed(String),
    Refused { had_token: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitEnd {
    Published(Report),
    Refused {
        had_token: bool,
    },
    TimedOut {
        last: Option<Report>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waited {
    pub end: WaitEnd,
    pub attempts: u32,
    pub elapsed: Duration,
}

impl Waited {
    pub fn exit_code(&self) -> i32 {
        match &self.end {
            WaitEnd::Published(_) => 0,
            WaitEnd::Refused { .. } => EXIT_REFUSED,
            WaitEnd::TimedOut { error: Some(_), .. } => EXIT_ERROR,
            WaitEnd::TimedOut { error: None, .. } => EXIT_NOT_PUBLISHED,
        }
    }

    pub fn elapsed_seconds(&self) -> u64 {
        whole_seconds(self.elapsed)
    }
}

fn whole_seconds(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_millis() >= 500)
}

pub fn wait_loop(
    plan: WaitPlan,
    mut attempt: impl FnMut() -> Attempt,
    mut pause: impl FnMut(u32, Duration, Option<&str>),
    mut now: impl FnMut() -> Duration,
) -> Waited {
    let mut attempts: u32 = 0;
    let mut last: Option<Report> = None;
    let mut error: Option<String>;
    loop {
        attempts = attempts.saturating_add(1);
        match attempt() {
            Attempt::Published(report) => {
                return Waited {
                    end: WaitEnd::Published(report),
                    attempts,
                    elapsed: now(),
                };
            }
            Attempt::Refused { had_token } => {
                return Waited {
                    end: WaitEnd::Refused { had_token },
                    attempts,
                    elapsed: now(),
                };
            }
            Attempt::Pending(report) => {
                last = Some(report);
                error = None;
            }
            Attempt::Failed(message) => error = Some(message),
        }
        let elapsed = now();
        if elapsed >= plan.timeout {
            return Waited {
                end: WaitEnd::TimedOut { last, error },
                attempts,
                elapsed,
            };
        }
        let delay = plan.interval.min(plan.timeout - elapsed);
        pause(attempts, delay, error.as_deref());
    }
}

pub fn render_timeout_text(report: &Report, elapsed_seconds: u64) -> String {
    let package = &report.package;
    let version = report.version.as_deref().unwrap_or_default();
    match &report.latest {
        Some(latest) => {
            format!("{package} {version}: not published after {elapsed_seconds}s (latest {latest})")
        }
        None => format!("{package} {version}: not published after {elapsed_seconds}s"),
    }
}

pub fn render_wait_json(report: &Report, waited: &Waited, timed_out: bool) -> Value {
    let mut value = render_json(report);
    if let Some(object) = value.as_object_mut() {
        object.insert("waited".to_string(), json!(true));
        object.insert("attempts".to_string(), json!(waited.attempts));
        object.insert(
            "elapsed_seconds".to_string(),
            json!(waited.elapsed_seconds()),
        );
        object.insert("timed_out".to_string(), json!(timed_out));
    }
    value
}

pub const NUGET_USER: &str = "pipetrain";

pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

pub fn basic(token: &str) -> String {
    format!(
        "Basic {}",
        base64(format!("{NUGET_USER}:{token}").as_bytes())
    )
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let block = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * index))
        });
        for position in 0..4 {
            if position <= chunk.len() {
                let index = (block >> (18 - 6 * position)) & 0x3f;
                out.push(char::from(ALPHABET[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn not_found_message(feed: &str) -> String {
    format!("package not found in {feed}")
}

pub fn refused_message(had_token: bool) -> String {
    if had_token {
        "the feed refused the token (401/403): it needs the read:packages scope".to_string()
    } else {
        "the feed refused the request (401/403) and no GitHub token was found: run `gh auth login` or set GH_TOKEN to a token with the read:packages scope".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn ecosystem_defaults_to_npm_for_scoped_packages() {
        assert_eq!(Ecosystem::default_for("@acme/ui"), Ecosystem::Npm);
        assert_eq!(Ecosystem::default_for("Acme.Util"), Ecosystem::Nuget);
    }

    #[test]
    fn npm_base_uses_github_for_scoped_package_with_org() {
        assert_eq!(npm_base("@acme/ui", Some("acme"), None), NPM_GITHUB);
        assert_eq!(npm_base("@acme/ui", None, None), NPM_PUBLIC);
        assert_eq!(npm_base("left-pad", Some("acme"), None), NPM_PUBLIC);
    }

    #[test]
    fn npm_base_prefers_registry_override() {
        assert_eq!(
            npm_base("@a/b", Some("a"), Some("http://127.0.0.1:9/")),
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn npm_url_encodes_the_scope_slash() {
        assert_eq!(
            npm_url("https://npm.pkg.github.com/", "@acme/ui"),
            "https://npm.pkg.github.com/@acme%2fui"
        );
        assert_eq!(
            npm_url("https://registry.npmjs.org", "left-pad"),
            "https://registry.npmjs.org/left-pad"
        );
    }

    #[test]
    fn nuget_index_url_needs_org_or_registry() {
        assert_eq!(
            nuget_index_url(Some("acme"), None).as_deref(),
            Some("https://nuget.pkg.github.com/acme/index.json")
        );
        assert_eq!(
            nuget_index_url(Some("acme"), Some("http://h/v3/index.json")).as_deref(),
            Some("http://h/v3/index.json")
        );
        assert_eq!(nuget_index_url(None, None), None);
    }

    #[test]
    fn nuget_versions_url_lowercases_the_id() {
        assert_eq!(
            nuget_versions_url("https://nuget.pkg.github.com/acme/download/", "Acme.Util"),
            "https://nuget.pkg.github.com/acme/download/acme.util/index.json"
        );
    }

    #[test]
    fn host_extraction_ignores_port_path_and_userinfo() {
        assert_eq!(host("https://npm.pkg.github.com/x"), "npm.pkg.github.com");
        assert_eq!(host("http://127.0.0.1:8080/a/b"), "127.0.0.1");
        assert_eq!(host("https://user:pw@example.com/a"), "example.com");
    }

    #[test]
    fn github_packages_hosts_are_recognised() {
        assert!(is_github_packages(
            "https://nuget.pkg.github.com/o/index.json"
        ));
        assert!(is_github_packages("https://npm.pkg.github.com/x"));
        assert!(!is_github_packages("https://registry.npmjs.org/x"));
        assert!(!is_github_packages("http://127.0.0.1:8080/x"));
        assert!(!is_github_packages(
            "https://evilpkg.github.com.example.com/x"
        ));
        assert!(!is_github_packages("https://notpkg.github.com/x"));
    }

    #[test]
    fn nuget_base_address_picks_the_package_base_address_resource() {
        let index = json!({
            "version": "3.0.0",
            "resources": [
                {"@id": "https://h/query", "@type": "SearchQueryService"},
                {"@id": "https://h/download/", "@type": "PackageBaseAddress/3.0.0"},
                {"@id": "https://h/publish", "@type": "PackagePublish/2.0.0"}
            ]
        });
        assert_eq!(
            nuget_base_address(&index).as_deref(),
            Some("https://h/download/")
        );
    }

    #[test]
    fn nuget_base_address_is_none_without_the_resource() {
        let index = json!({"resources": [{"@id": "https://h/q", "@type": "SearchQueryService"}]});
        assert_eq!(nuget_base_address(&index), None);
        assert_eq!(nuget_base_address(&json!({})), None);
    }

    #[test]
    fn npm_packument_versions_are_the_keys() {
        let body = json!({"name": "x", "versions": {"1.0.0": {}, "1.1.0": {}}});
        let mut found = npm_versions(&body);
        found.sort();
        assert_eq!(found, strings(&["1.0.0", "1.1.0"]));
        assert!(npm_versions(&json!({"name": "x"})).is_empty());
    }

    #[test]
    fn nuget_versions_are_read_from_the_array() {
        let body = json!({"versions": ["1.0.0", "1.2610.2314.49"]});
        assert_eq!(nuget_versions(&body), strings(&["1.0.0", "1.2610.2314.49"]));
        assert!(nuget_versions(&json!({})).is_empty());
    }

    #[test]
    fn versions_compare_numerically_by_segment() {
        assert_eq!(compare("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare("2.0.0", "10.0.0"), Ordering::Less);
    }

    #[test]
    fn four_part_versions_compare() {
        assert_eq!(
            compare("1.2610.2314.49", "1.2610.2314.9"),
            Ordering::Greater
        );
        assert_eq!(compare("1.2610.2314.49", "1.2610.2315.1"), Ordering::Less);
        assert_eq!(compare("1.2610.2314.0", "1.2610.2314"), Ordering::Equal);
    }

    #[test]
    fn prerelease_sorts_below_the_same_numeric_version() {
        assert_eq!(compare("1.0.0-beta.1", "1.0.0"), Ordering::Less);
        assert_eq!(compare("1.0.1-alpha", "1.0.0"), Ordering::Greater);
        assert_eq!(compare("1.0.0-beta.2", "1.0.0-beta.10"), Ordering::Less);
        assert_eq!(compare("1.0.0-alpha", "1.0.0-beta"), Ordering::Less);
    }

    #[test]
    fn build_metadata_and_leading_v_are_ignored() {
        assert_eq!(compare("v1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare("1.2.3+abc", "1.2.3"), Ordering::Equal);
    }

    #[test]
    fn newest_first_orders_and_dedupes() {
        let found = strings(&["1.0.0", "1.10.0", "1.9.0", "1.10.0", "1.10.0-rc.1"]);
        assert_eq!(
            newest_first(&found),
            strings(&["1.10.0", "1.10.0-rc.1", "1.9.0", "1.0.0"])
        );
    }

    #[test]
    fn report_marks_a_present_version_as_published() {
        let report = Report::build(
            "Acme.Util",
            Ecosystem::Nuget,
            "https://f/index.json",
            Some("1.2610.2314.49"),
            &strings(&["1.2610.2314.49", "1.0.0"]),
        );
        assert_eq!(report.published, Some(true));
        assert_eq!(report.exit_code(), 0);
        assert_eq!(render_text(&report), "Acme.Util 1.2610.2314.49: published");
    }

    #[test]
    fn report_marks_a_missing_version_as_not_published() {
        let report = Report::build(
            "Acme.Util",
            Ecosystem::Nuget,
            "https://f/index.json",
            Some("2.0.0"),
            &strings(&["1.0.0", "1.5.0"]),
        );
        assert_eq!(report.published, Some(false));
        assert_eq!(report.exit_code(), EXIT_NOT_PUBLISHED);
        assert_eq!(
            render_text(&report),
            "Acme.Util 2.0.0: not published (latest 1.5.0)"
        );
    }

    #[test]
    fn report_without_a_version_lists_latest_and_recent() {
        let found = strings(&[
            "1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0", "1.5.0", "1.6.0",
        ]);
        let report = Report::build("left-pad", Ecosystem::Npm, "https://r", None, &found);
        assert_eq!(report.published, None);
        assert_eq!(report.exit_code(), 0);
        assert_eq!(report.latest.as_deref(), Some("1.6.0"));
        assert_eq!(report.versions.len(), RECENT);
        assert_eq!(
            render_text(&report),
            "left-pad: latest 1.6.0\nlast 5: 1.6.0, 1.5.0, 1.4.0, 1.3.0, 1.2.0"
        );
    }

    #[test]
    fn report_with_no_versions_says_so() {
        let none = Report::build("left-pad", Ecosystem::Npm, "https://r", None, &[]);
        assert_eq!(render_text(&none), "left-pad: no versions published");
        let asked = Report::build("left-pad", Ecosystem::Npm, "https://r", Some("1.0.0"), &[]);
        assert_eq!(asked.published, Some(false));
        assert_eq!(
            render_text(&asked),
            "left-pad 1.0.0: not published (no versions)"
        );
    }

    #[test]
    fn json_has_the_documented_shape() {
        let report = Report::build(
            "left-pad",
            Ecosystem::Npm,
            "https://r",
            None,
            &strings(&["1.0.0", "1.1.0"]),
        );
        assert_eq!(
            render_json(&report),
            json!({
                "schema": 1,
                "package": "left-pad",
                "ecosystem": "npm",
                "feed": "https://r",
                "version": null,
                "published": null,
                "latest": "1.1.0",
                "versions": ["1.1.0", "1.0.0"],
            })
        );
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn auth_headers_have_the_expected_shape() {
        assert_eq!(bearer("tok"), "Bearer tok");
        assert_eq!(basic("tok"), format!("Basic {}", base64(b"pipetrain:tok")));
    }

    #[test]
    fn refusal_messages_name_the_scope() {
        assert!(refused_message(true).contains("read:packages"));
        assert!(refused_message(false).contains("read:packages"));
        assert!(refused_message(false).contains("no GitHub token"));
        assert_eq!(
            not_found_message("https://r"),
            "package not found in https://r"
        );
    }

    fn pending_report() -> Report {
        Report::build(
            "left-pad",
            Ecosystem::Npm,
            "https://r",
            Some("2.0.0"),
            &strings(&["1.0.0"]),
        )
    }

    fn published_report() -> Report {
        Report::build(
            "left-pad",
            Ecosystem::Npm,
            "https://r",
            Some("2.0.0"),
            &strings(&["2.0.0", "1.0.0"]),
        )
    }

    fn seconds(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    struct Run {
        waited: Waited,
        pauses: Vec<(u32, Duration, Option<String>)>,
    }

    fn drive(plan: WaitPlan, script: Vec<Attempt>) -> Run {
        use std::cell::{Cell, RefCell};
        let clock = Cell::new(Duration::ZERO);
        let pauses = RefCell::new(Vec::new());
        let queue = RefCell::new(script.into_iter());
        let waited = wait_loop(
            plan,
            || {
                queue
                    .borrow_mut()
                    .next()
                    .unwrap_or(Attempt::Pending(pending_report()))
            },
            |attempts, delay, error| {
                pauses
                    .borrow_mut()
                    .push((attempts, delay, error.map(str::to_string)));
                clock.set(clock.get() + delay);
            },
            || clock.get(),
        );
        Run {
            waited,
            pauses: pauses.into_inner(),
        }
    }

    fn plan(interval: u64, timeout: u64) -> WaitPlan {
        WaitPlan {
            interval: seconds(interval),
            timeout: seconds(timeout),
        }
    }

    #[test]
    fn wait_returns_at_once_when_already_published() {
        let run = drive(plan(10, 600), vec![Attempt::Published(published_report())]);
        assert_eq!(run.waited.attempts, 1);
        assert_eq!(run.waited.exit_code(), 0);
        assert!(run.pauses.is_empty());
        assert!(matches!(run.waited.end, WaitEnd::Published(_)));
    }

    #[test]
    fn wait_polls_until_the_version_appears() {
        let run = drive(
            plan(10, 600),
            vec![
                Attempt::Pending(pending_report()),
                Attempt::Pending(pending_report()),
                Attempt::Published(published_report()),
            ],
        );
        assert_eq!(run.waited.attempts, 3);
        assert_eq!(run.waited.exit_code(), 0);
        assert_eq!(run.waited.elapsed, seconds(20));
        assert_eq!(
            run.pauses,
            vec![(1, seconds(10), None), (2, seconds(10), None)]
        );
    }

    #[test]
    fn wait_times_out_while_pending_with_exit_one() {
        let run = drive(plan(10, 25), vec![]);
        assert_eq!(run.waited.exit_code(), EXIT_NOT_PUBLISHED);
        assert_eq!(run.waited.attempts, 4);
        assert_eq!(run.waited.elapsed, seconds(25));
        match run.waited.end {
            WaitEnd::TimedOut { last, error } => {
                assert_eq!(last, Some(pending_report()));
                assert_eq!(error, None);
            }
            other => panic!("unexpected end: {other:?}"),
        }
    }

    #[test]
    fn wait_caps_the_pause_at_the_remaining_time() {
        let run = drive(plan(10, 25), vec![]);
        let delays: Vec<Duration> = run.pauses.iter().map(|pause| pause.1).collect();
        assert_eq!(delays, vec![seconds(10), seconds(10), seconds(5)]);
    }

    #[test]
    fn wait_with_timeout_zero_checks_once() {
        let run = drive(plan(10, 0), vec![]);
        assert_eq!(run.waited.attempts, 1);
        assert!(run.pauses.is_empty());
        assert_eq!(run.waited.exit_code(), EXIT_NOT_PUBLISHED);
    }

    #[test]
    fn wait_with_timeout_zero_still_reports_a_published_version() {
        let run = drive(plan(10, 0), vec![Attempt::Published(published_report())]);
        assert_eq!(run.waited.attempts, 1);
        assert_eq!(run.waited.exit_code(), 0);
    }

    #[test]
    fn wait_retries_errors_and_exits_three_at_the_timeout() {
        let failed = || Attempt::Failed("connection refused".to_string());
        let run = drive(plan(10, 20), vec![failed(), failed(), failed()]);
        assert_eq!(run.waited.exit_code(), EXIT_ERROR);
        assert_eq!(run.waited.attempts, 3);
        assert_eq!(run.pauses[0].2.as_deref(), Some("connection refused"));
        match run.waited.end {
            WaitEnd::TimedOut { last, error } => {
                assert_eq!(last, None);
                assert_eq!(error.as_deref(), Some("connection refused"));
            }
            other => panic!("unexpected end: {other:?}"),
        }
    }

    #[test]
    fn wait_recovers_from_an_error_and_then_times_out_as_not_published() {
        let run = drive(
            plan(10, 20),
            vec![
                Attempt::Failed("timed out".to_string()),
                Attempt::Pending(pending_report()),
                Attempt::Pending(pending_report()),
            ],
        );
        assert_eq!(run.waited.exit_code(), EXIT_NOT_PUBLISHED);
        assert_eq!(run.pauses[0].2.as_deref(), Some("timed out"));
        assert_eq!(run.pauses[1].2, None);
    }

    #[test]
    fn wait_ends_on_error_after_pending_when_the_last_attempt_failed() {
        let run = drive(
            plan(10, 20),
            vec![
                Attempt::Pending(pending_report()),
                Attempt::Failed("http 500".to_string()),
                Attempt::Failed("http 500".to_string()),
            ],
        );
        assert_eq!(run.waited.exit_code(), EXIT_ERROR);
    }

    #[test]
    fn wait_can_publish_after_an_error() {
        let run = drive(
            plan(10, 600),
            vec![
                Attempt::Failed("http 502".to_string()),
                Attempt::Published(published_report()),
            ],
        );
        assert_eq!(run.waited.exit_code(), 0);
        assert_eq!(run.waited.attempts, 2);
    }

    #[test]
    fn wait_aborts_at_once_when_refused() {
        let run = drive(
            plan(10, 600),
            vec![
                Attempt::Pending(pending_report()),
                Attempt::Refused { had_token: true },
            ],
        );
        assert_eq!(run.waited.exit_code(), EXIT_REFUSED);
        assert_eq!(run.waited.attempts, 2);
        assert_eq!(run.pauses.len(), 1);
        assert_eq!(run.waited.end, WaitEnd::Refused { had_token: true });
    }

    #[test]
    fn exit_codes_are_distinct() {
        let codes = [0, EXIT_NOT_PUBLISHED, EXIT_REFUSED, EXIT_ERROR];
        assert_eq!(codes, [0, 1, 2, 3]);
    }

    #[test]
    fn elapsed_seconds_rounds_to_the_nearest_second() {
        let waited = |elapsed| Waited {
            end: WaitEnd::Refused { had_token: false },
            attempts: 1,
            elapsed,
        };
        assert_eq!(waited(Duration::from_millis(1400)).elapsed_seconds(), 1);
        assert_eq!(waited(Duration::from_millis(1500)).elapsed_seconds(), 2);
        assert_eq!(waited(Duration::ZERO).elapsed_seconds(), 0);
    }

    #[test]
    fn timeout_text_names_the_latest_version_when_there_is_one() {
        assert_eq!(
            render_timeout_text(&pending_report(), 600),
            "left-pad 2.0.0: not published after 600s (latest 1.0.0)"
        );
    }

    #[test]
    fn timeout_text_omits_latest_when_there_are_no_versions() {
        let empty = Report::build("left-pad", Ecosystem::Npm, "https://r", Some("2.0.0"), &[]);
        assert_eq!(
            render_timeout_text(&empty, 30),
            "left-pad 2.0.0: not published after 30s"
        );
    }

    #[test]
    fn wait_json_adds_fields_and_keeps_schema_one() {
        let waited = Waited {
            end: WaitEnd::TimedOut {
                last: Some(pending_report()),
                error: None,
            },
            attempts: 4,
            elapsed: Duration::from_millis(30_200),
        };
        let value = render_wait_json(&pending_report(), &waited, true);
        assert_eq!(value["schema"], 1);
        assert_eq!(value["published"], false);
        assert_eq!(value["waited"], true);
        assert_eq!(value["attempts"], 4);
        assert_eq!(value["elapsed_seconds"], 30);
        assert_eq!(value["timed_out"], true);
        let mut base = value.clone();
        for key in ["waited", "attempts", "elapsed_seconds", "timed_out"] {
            base.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(base, render_json(&pending_report()));
    }
}
