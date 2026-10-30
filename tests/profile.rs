use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use pipetrain::command::init;
use pipetrain::command::orgs::{self, Visible};
use pipetrain::command::profiles;
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::github::Client;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered, Plan, ProfileInfo};
use pipetrain::profile::{self, Inputs, ProfileSet, ProfileSource};
use pipetrain::publishers::Publishers;
use pipetrain::render;
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

const MULTI: &str = r#"
default_profile = "acme"
max_parallel = 3
train_prefix = "release/"
allow_missing_checks = true

[profile.acme]
orgs = ["acme"]
folders = ["/work/acme"]

[profile.globex]
orgs = ["globex", "globex-labs"]
folders = ["/work/globex"]
max_parallel = 7

[profile.labs]
orgs = ["globex-labs"]
folders = ["/work/globex/labs"]
train_prefix = "deploy/"
"#;

fn set(text: &str) -> ProfileSet {
    ProfileSet::parse(text).unwrap()
}

fn error_of(text: &str) -> String {
    format!("{:#}", ProfileSet::parse(text).unwrap_err())
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

fn pick(
    set: &ProfileSet,
    flag: Option<&str>,
    arg_orgs: &[&str],
    cwd: Option<&str>,
) -> (String, ProfileSource) {
    let orgs = strings(arg_orgs);
    let inputs = Inputs {
        flag,
        arg_orgs: &orgs,
        cwd,
        case_insensitive: false,
    };
    let found = profile::select(set, &inputs).unwrap();
    (found.profile.name.clone(), found.source)
}

fn pick_error(
    set: &ProfileSet,
    flag: Option<&str>,
    arg_orgs: &[&str],
    cwd: Option<&str>,
) -> String {
    let orgs = strings(arg_orgs);
    let inputs = Inputs {
        flag,
        arg_orgs: &orgs,
        cwd,
        case_insensitive: false,
    };
    format!("{:#}", profile::select(set, &inputs).unwrap_err())
}

#[test]
fn a_flat_config_is_the_default_profile() {
    let set = set("orgs = [\"acme\"]\nmax_parallel = 2\n");
    assert_eq!(set.profiles.len(), 1);
    assert_eq!(set.profiles[0].name, "default");
    assert_eq!(set.profiles[0].config.orgs, vec!["acme"]);
    assert_eq!(set.profiles[0].config.max_parallel, 2);
    assert!(set.profiles[0].folders.is_empty());
    assert_eq!(set.default_profile, None);
}

#[test]
fn a_flat_config_still_parses_through_config_parse() {
    let cfg = Config::parse("orgs = [\"acme\"]\nmax_parallel = 2\n").unwrap();
    assert_eq!(cfg.orgs, vec!["acme"]);
    assert_eq!(cfg.max_parallel, 2);
}

#[test]
fn an_empty_config_is_the_default_profile() {
    let set = set("");
    assert_eq!(set.profiles.len(), 1);
    assert_eq!(set.profiles[0].name, "default");
}

#[test]
fn a_flat_config_may_name_default_as_the_default_profile() {
    let set = set("default_profile = \"default\"\norgs = [\"acme\"]\n");
    assert_eq!(set.default_profile.as_deref(), Some("default"));
}

#[test]
fn profiles_inherit_the_shared_keys_and_override_them() {
    let set = set(MULTI);
    let names: Vec<&str> = set.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["acme", "globex", "labs"]);
    let acme = &set.find("acme").unwrap().config;
    assert_eq!(acme.max_parallel, 3);
    assert_eq!(acme.train_prefix, "release/");
    assert!(acme.allow_missing_checks);
    assert_eq!(acme.orgs, vec!["acme"]);
    let globex = &set.find("globex").unwrap().config;
    assert_eq!(globex.max_parallel, 7);
    assert_eq!(globex.train_prefix, "release/");
    assert_eq!(globex.orgs, vec!["globex", "globex-labs"]);
    let labs = &set.find("labs").unwrap().config;
    assert_eq!(labs.train_prefix, "deploy/");
    assert_eq!(labs.max_parallel, 3);
    assert_eq!(set.default_profile.as_deref(), Some("acme"));
}

#[test]
fn a_profile_reads_its_folders() {
    let set = set(MULTI);
    assert_eq!(set.find("labs").unwrap().folders, vec!["/work/globex/labs"]);
}

#[test]
fn shared_layers_and_orders_reach_every_profile() {
    let text = "[[layer]]\nname = \"api\"\nrepos = [\"acme/api-*\"]\n[[order]]\nrepos = [\"acme/a\", \"acme/b\"]\n[profile.one]\norgs = [\"acme\"]\n[profile.two]\norgs = [\"globex\"]\n";
    let set = set(text);
    for profile in &set.profiles {
        assert_eq!(profile.config.layer.len(), 1);
        assert_eq!(profile.config.order.len(), 1);
    }
}

#[test]
fn a_profile_can_replace_the_shared_layers() {
    let text = "[[layer]]\nname = \"api\"\nrepos = [\"acme/api-*\"]\n[profile.one]\norgs = [\"acme\"]\n[[profile.two.layer]]\nname = \"web\"\nrepos = [\"globex/web-*\"]\n[profile.two]\norgs = [\"globex\"]\n";
    let set = set(text);
    assert_eq!(set.find("one").unwrap().config.layer[0].name, "api");
    assert_eq!(set.find("two").unwrap().config.layer[0].name, "web");
}

#[test]
fn config_parse_refuses_a_multi_profile_file() {
    let message = format!("{:#}", Config::parse(MULTI).unwrap_err());
    assert!(message.contains("3 profiles"));
}

#[test]
fn an_unknown_top_level_key_is_still_rejected() {
    let message = error_of("orgs = [\"acme\"]\nmax_paralel = 2\n");
    assert!(message.contains("max_paralel"));
}

#[test]
fn an_unknown_key_in_a_profile_names_the_profile_and_the_key() {
    let message = error_of("[profile.acme]\norgs = [\"acme\"]\nmax_paralel = 2\n");
    assert!(message.contains("acme"));
    assert!(message.contains("max_paralel"));
}

#[test]
fn an_empty_profile_name_is_rejected() {
    let message = error_of("[profile.\"\"]\norgs = [\"acme\"]\n");
    assert!(message.contains("empty name"));
}

#[test]
fn profile_names_differing_only_by_case_are_duplicates() {
    let message = error_of("[profile.Acme]\norgs = [\"a\"]\n[profile.acme]\norgs = [\"b\"]\n");
    assert!(message.contains("defined twice"));
}

#[test]
fn a_default_profile_naming_nothing_is_rejected() {
    let message = error_of("default_profile = \"nope\"\n[profile.acme]\norgs = [\"acme\"]\n");
    assert!(message.contains("default_profile"));
    assert!(message.contains("nope"));
    assert!(message.contains("acme"));
}

#[test]
fn a_flat_config_rejects_a_default_profile_other_than_default() {
    let message = error_of("default_profile = \"nope\"\norgs = [\"acme\"]\n");
    assert!(message.contains("default_profile"));
    assert!(message.contains("flat config"));
}

#[test]
fn a_default_profile_must_be_a_string() {
    let message = error_of("default_profile = 3\n[profile.acme]\norgs = [\"acme\"]\n");
    assert!(message.contains("default_profile"));
}

#[test]
fn folders_outside_a_profile_are_rejected() {
    let message = error_of("folders = [\"/work\"]\n[profile.acme]\norgs = [\"acme\"]\n");
    assert!(message.contains("folders"));
    let flat = error_of("folders = [\"/work\"]\n");
    assert!(flat.contains("folders"));
}

#[test]
fn folders_must_be_a_list_of_text() {
    let not_list = error_of("[profile.acme]\nfolders = \"/work\"\n");
    assert!(not_list.contains("profile.acme.folders"));
    let not_text = error_of("[profile.acme]\nfolders = [3]\n");
    assert!(not_text.contains("profile.acme.folders"));
    let blank = error_of("[profile.acme]\nfolders = [\"\"]\n");
    assert!(blank.contains("profile.acme.folders"));
}

#[test]
fn a_folder_cannot_belong_to_two_profiles() {
    let text = "[profile.a]\nfolders = [\"/work/x\"]\n[profile.b]\nfolders = [\"/work\\\\x\"]\n";
    let message = error_of(text);
    assert!(message.contains("is listed in profiles `a` and `b`"));
}

#[test]
fn profile_must_be_a_set_of_tables() {
    let message = error_of("profile = 3\n");
    assert!(message.contains("profile"));
    let inner = error_of("[profile]\nacme = 3\n");
    assert!(inner.contains("profile.acme"));
}

#[test]
fn an_invalid_value_inside_a_profile_names_the_profile() {
    let message = error_of("[profile.acme]\nmax_parallel = \"many\"\n");
    assert!(message.contains("profile `acme`"));
}

fn fixtures() -> ProfileSet {
    set(MULTI)
}

#[test]
fn the_flag_wins_over_everything() {
    let set = fixtures();
    let picked = pick(&set, Some("globex"), &["acme"], Some("/work/acme/api"));
    assert_eq!(picked, ("globex".to_string(), ProfileSource::Flag));
}

#[test]
fn the_flag_ignores_case() {
    let picked = pick(&fixtures(), Some("GLOBEX"), &[], None);
    assert_eq!(picked.0, "globex");
}

#[test]
fn an_unknown_flag_lists_the_profiles() {
    let message = pick_error(&fixtures(), Some("nope"), &[], None);
    assert!(message.contains("unknown profile `nope`"));
    assert!(message.contains("acme"));
    assert!(message.contains("globex"));
}

#[test]
fn args_pick_the_one_profile_that_owns_every_org() {
    let picked = pick(
        &fixtures(),
        None,
        &["globex", "globex-labs"],
        Some("/work/acme"),
    );
    assert_eq!(picked, ("globex".to_string(), ProfileSource::Args));
}

#[test]
fn args_beat_the_folder() {
    let picked = pick(&fixtures(), None, &["acme"], Some("/work/globex"));
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Args));
}

#[test]
fn args_ignore_case_in_org_names() {
    let picked = pick(&fixtures(), None, &["ACME"], None);
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Args));
}

#[test]
fn args_shared_by_two_profiles_fall_through_to_the_folder() {
    let picked = pick(&fixtures(), None, &["globex-labs"], Some("/work/acme"));
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Folder));
}

#[test]
fn args_spanning_two_profiles_fall_through() {
    let picked = pick(&fixtures(), None, &["acme", "globex"], Some("/work/globex"));
    assert_eq!(picked, ("globex".to_string(), ProfileSource::Folder));
}

#[test]
fn the_deepest_folder_wins() {
    let set = fixtures();
    let deep = pick(&set, None, &[], Some("/work/globex/labs/tool"));
    assert_eq!(deep, ("labs".to_string(), ProfileSource::Folder));
    let shallow = pick(&set, None, &[], Some("/work/globex/other"));
    assert_eq!(shallow, ("globex".to_string(), ProfileSource::Folder));
}

#[test]
fn the_folder_itself_matches() {
    let picked = pick(&fixtures(), None, &[], Some("/work/acme"));
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Folder));
}

#[test]
fn a_folder_prefix_must_end_on_a_path_boundary() {
    let set = fixtures();
    let picked = pick(&set, None, &[], Some("/work/acme-other"));
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Default));
}

#[test]
fn separators_are_normalized() {
    let text =
        "[profile.win]\nfolders = [\"X:\\\\Workspace\"]\n[profile.other]\nfolders = [\"/opt\"]\n";
    let set = set(text);
    let picked = pick(&set, None, &[], Some("X:/Workspace/api"));
    assert_eq!(picked, ("win".to_string(), ProfileSource::Folder));
}

#[test]
fn folder_matching_is_case_insensitive_when_asked() {
    let text =
        "[profile.win]\nfolders = [\"X:\\\\Workspace\"]\n[profile.other]\nfolders = [\"/opt\"]\n";
    let set = set(text);
    let orgs: Vec<String> = Vec::new();
    let windows = Inputs {
        flag: None,
        arg_orgs: &orgs,
        cwd: Some("x:\\workspace\\Api"),
        case_insensitive: true,
    };
    let found = profile::select(&set, &windows).unwrap();
    assert_eq!(found.profile.name, "win");
    assert_eq!(found.source, ProfileSource::Folder);
    let posix = Inputs {
        case_insensitive: false,
        ..windows
    };
    assert!(profile::select(&set, &posix).is_err());
}

#[test]
fn folder_depth_counts_components() {
    assert_eq!(profile::folder_depth("/a/b", "/a/b/c", false), Some(2));
    assert_eq!(profile::folder_depth("/a/b/c", "/a/b", false), None);
    assert_eq!(profile::folder_depth("/a/B", "/a/b", false), None);
    assert_eq!(profile::folder_depth("/a/B", "/a/b", true), Some(2));
    assert_eq!(profile::folder_depth("", "/a/b", false), None);
}

#[test]
fn the_default_profile_is_used_outside_every_folder() {
    let picked = pick(&fixtures(), None, &[], Some("/elsewhere"));
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Default));
}

#[test]
fn a_missing_working_folder_falls_to_the_default() {
    let picked = pick(&fixtures(), None, &[], None);
    assert_eq!(picked, ("acme".to_string(), ProfileSource::Default));
}

#[test]
fn the_only_profile_is_used_without_a_default() {
    let set = set("[profile.solo]\norgs = [\"acme\"]\n");
    let picked = pick(&set, None, &[], Some("/anywhere"));
    assert_eq!(picked, ("solo".to_string(), ProfileSource::Only));
}

#[test]
fn a_flat_config_resolves_as_the_only_profile() {
    let set = set("orgs = [\"acme\"]\n");
    let picked = pick(&set, None, &[], Some("/anywhere"));
    assert_eq!(picked, ("default".to_string(), ProfileSource::Only));
}

#[test]
fn ambiguity_lists_the_profiles_and_how_to_choose() {
    let text = "[profile.acme]\norgs = [\"acme\"]\n[profile.globex]\norgs = [\"globex\", \"globex-labs\"]\n";
    let message = pick_error(&set(text), None, &[], Some("/anywhere"));
    assert!(message.contains("acme  orgs: acme"));
    assert!(message.contains("globex  orgs: globex, globex-labs"));
    assert!(message.contains("--profile NAME"));
}

#[test]
fn profiles_are_never_merged_across_orgs() {
    let set = fixtures();
    let found = pick(&set, None, &[], Some("/work/acme"));
    let chosen = set.find(&found.0).unwrap();
    assert_eq!(chosen.config.orgs, vec!["acme"]);
}

#[test]
fn the_profiles_table_marks_the_chosen_profile() {
    let set = fixtures();
    let picked = profiles::chosen(&set, Some("/work/globex"));
    assert_eq!(picked, Some(("globex".to_string(), ProfileSource::Folder)));
    let table = profiles::table(&set, picked.as_ref());
    let marked: Vec<&str> = table.lines().filter(|l| l.starts_with('*')).collect();
    assert_eq!(marked.len(), 1);
    assert!(marked[0].contains("globex"));
    assert!(marked[0].contains("globex, globex-labs"));
    assert!(table.contains("/work/globex/labs"));
    assert_eq!(
        profiles::footer(picked.as_ref()),
        "from here: globex (from the working folder)"
    );
}

#[test]
fn the_profiles_footer_says_when_nothing_resolves() {
    let text = "[profile.a]\norgs = [\"a\"]\n[profile.b]\norgs = [\"b\"]\n";
    let set = set(text);
    let picked = profiles::chosen(&set, Some("/anywhere"));
    assert_eq!(picked, None);
    assert!(profiles::footer(None).contains("--profile NAME"));
}

fn pr(number: u64, slug: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: "feat/x".to_string(),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
    }
}

fn built() -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: strings(&["acme"]),
            ready: vec![(pr(1, "acme/api"), "explicit".to_string())],
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 1,
            rate_remaining: Some(4000),
        },
    )
}

fn with_profile(source: ProfileSource) -> Plan {
    let mut plan = built();
    plan.profile = Some(ProfileInfo {
        name: "acme".to_string(),
        source,
    });
    plan
}

#[test]
fn json_gets_a_profile_object_and_nothing_else_changes() {
    let bare = serde_json::to_value(built()).unwrap();
    let tagged = serde_json::to_value(with_profile(ProfileSource::Folder)).unwrap();
    assert!(bare.get("profile").is_none());
    assert_eq!(
        tagged["profile"],
        json!({ "name": "acme", "source": "folder" })
    );
    assert_eq!(tagged["schema"], json!(1));
    let mut stripped = tagged.clone();
    stripped.as_object_mut().unwrap().remove("profile");
    assert_eq!(stripped, bare);
}

#[test]
fn every_source_serializes_as_a_lowercase_word() {
    let cases = [
        (ProfileSource::Flag, "flag"),
        (ProfileSource::Args, "args"),
        (ProfileSource::Folder, "folder"),
        (ProfileSource::Default, "default"),
        (ProfileSource::Only, "only"),
    ];
    for (source, word) in cases {
        let value = serde_json::to_value(with_profile(source)).unwrap();
        assert_eq!(value["profile"]["source"], json!(word));
        assert_eq!(source.as_str(), word);
    }
}

#[test]
fn the_text_header_names_the_profile_and_where_it_came_from() {
    let text = render::text(&with_profile(ProfileSource::Folder));
    assert!(text.contains("acme (from the working folder)"));
    let bare = render::text(&built());
    assert!(!bare.contains("profile  "));
}

#[test]
fn the_proposal_has_one_profile_per_org() {
    let text = init::proposal(&strings(&["acme", "acme-labs", "9lives"]));
    assert!(text.contains("[profile.acme]\norgs = [\"acme\"]\n# folders = ["));
    assert!(text.contains("[profile.acme-labs]\norgs = [\"acme-labs\"]\n"));
    assert!(text.contains("[profile.9lives]\n"));
    assert_eq!(text.matches("# folders = [").count(), 3);
    let parsed = set(&text);
    let names: Vec<&str> = parsed.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["9lives", "acme", "acme-labs"]);
    assert_eq!(
        parsed.find("acme-labs").unwrap().config.orgs,
        vec!["acme-labs"]
    );
    assert!(parsed.profiles.iter().all(|p| p.folders.is_empty()));
}

#[test]
fn the_proposal_quotes_names_that_are_not_bare_keys() {
    let text = init::proposal(&strings(&["odd.org"]));
    assert!(text.contains("[profile.\"odd.org\"]"));
    let parsed = set(&text);
    assert_eq!(parsed.profiles[0].name, "odd.org");
}

#[test]
fn the_proposal_with_no_orgs_is_still_valid_config() {
    let text = init::proposal(&[]);
    assert!(ProfileSet::parse(&text).is_ok());
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pipetrain-profile-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn init_refuses_to_overwrite_without_force() {
    let dir = scratch("refuse");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, "orgs = [\"keep\"]\n").unwrap();
    let message = format!(
        "{:#}",
        init::write_config(&path, "orgs = [\"new\"]\n", false).unwrap_err()
    );
    assert!(message.contains("already exists"));
    assert!(message.contains("--force"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "orgs = [\"keep\"]\n");
    init::write_config(&path, "orgs = [\"new\"]\n", true).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "orgs = [\"new\"]\n");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_creates_missing_folders_when_writing() {
    let dir = scratch("create");
    let path = dir.join("nested").join("config.toml");
    init::write_config(&path, "orgs = [\"acme\"]\n", false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "orgs = [\"acme\"]\n");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_default_write_path_lives_in_the_user_config_dir() {
    let path = init::default_path().unwrap();
    assert!(path.ends_with(PathBuf::from("pipetrain").join("config.toml")));
}

struct Canned {
    status: &'static str,
    body: String,
}

fn canned(status: &'static str, body: Value) -> Canned {
    Canned {
        status,
        body: body.to_string(),
    }
}

fn serve(replies: Vec<Canned>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            seen.push(read_request(&mut stream));
            stream.write_all(render_reply(&reply).as_bytes()).unwrap();
        }
        seen
    });
    (base, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if read == 0 || body_complete(&text) {
            return text;
        }
    }
}

fn body_complete(text: &str) -> bool {
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let wanted = head
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    body.len() >= wanted
}

fn render_reply(reply: &Canned) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    )
}

fn client(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

fn page(viewer: bool, logins: &[&str], next: Option<&str>) -> Canned {
    let nodes: Vec<Value> = logins.iter().map(|l| json!({ "login": l })).collect();
    let mut data = json!({
        "viewer": {
            "organizations": {
                "nodes": nodes,
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next }
            }
        }
    });
    if viewer {
        data["viewer"]["login"] = json!("octocat");
    }
    canned("200 OK", json!({ "data": data }))
}

#[test]
fn orgs_follow_the_pagination_and_sort_without_case() {
    let (base, handle) = serve(vec![
        page(true, &["Zeta", "acme"], Some("cursor-1")),
        page(false, &["beta", "acme"], None),
    ]);
    let visible = orgs::fetch(&mut client(&base)).unwrap();
    let seen = handle.join().unwrap();
    assert_eq!(visible.viewer.as_deref(), Some("octocat"));
    assert_eq!(visible.orgs, vec!["acme", "beta", "Zeta"]);
    assert_eq!(seen.len(), 2);
    assert!(seen[0].contains("organizations"));
    assert!(seen[1].contains("cursor-1"));
    assert!(seen.iter().all(|request| !request.contains("mutation")));
}

#[test]
fn orgs_stop_at_a_single_page() {
    let (base, handle) = serve(vec![page(true, &["acme"], None)]);
    let visible = orgs::fetch(&mut client(&base)).unwrap();
    assert_eq!(handle.join().unwrap().len(), 1);
    assert_eq!(visible.orgs, vec!["acme"]);
}

#[test]
fn orgs_surface_an_api_error() {
    let (base, handle) = serve(vec![canned(
        "401 Unauthorized",
        json!({ "message": "Bad credentials" }),
    )]);
    assert!(orgs::fetch(&mut client(&base)).is_err());
    handle.join().unwrap();
}

#[test]
fn orgs_parse_tolerates_a_body_without_organizations() {
    let page = orgs::parse(&json!({ "data": { "viewer": { "login": "octocat" } } }));
    assert_eq!(page.viewer.as_deref(), Some("octocat"));
    assert!(page.logins.is_empty());
    assert_eq!(page.next, None);
}

#[test]
fn orgs_render_says_which_profile_has_each_org() {
    let visible = Visible {
        viewer: Some("octocat".to_string()),
        orgs: strings(&["acme", "beta", "globex-labs"]),
    };
    let text = orgs::render(&visible, &fixtures());
    assert!(text.contains("signed in as octocat"));
    assert!(text.contains("3 organizations visible"));
    assert!(text.contains("acme  profile: acme"));
    assert!(text.contains("beta  (not in any profile)"));
    assert!(text.contains("globex-labs  profile: globex, labs"));
}
