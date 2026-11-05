use std::path::{Path, PathBuf};

use pipetrain::doctor::check::{self, Status};

fn place(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("pipetrain-configs-never-created")
        .join(name)
        .join("pipetrain.toml")
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn no_config_is_ok_and_lists_the_places_searched() {
    let (a, b) = (place("a"), place("b"));
    let c = check::configs(&[a.clone(), b.clone()], &[], None);
    assert_eq!(c.id, "configs");
    assert_eq!(c.status, Status::Ok);
    assert_eq!(
        c.message,
        format!(
            "no config file in the places searched: {}, then {}",
            text(&a),
            text(&b)
        )
    );
    assert_eq!(c.fix, None);
}

#[test]
fn a_single_config_is_ok_and_called_the_only_one() {
    let (a, b) = (place("a"), place("b"));
    let c = check::configs(&[a.clone(), b.clone()], std::slice::from_ref(&b), None);
    assert_eq!(c.status, Status::Ok);
    assert_eq!(c.message, format!("{} (the only one found)", text(&b)));
    assert_eq!(c.fix, None);
}

#[test]
fn two_configs_warn_name_the_used_one_and_the_ignored_one() {
    let (a, b) = (place("a"), place("b"));
    let searched = [a.clone(), b.clone()];
    let c = check::configs(&searched, &searched, None);
    assert_eq!(c.status, Status::Warn);
    assert!(c.message.starts_with("2 configs found."));
    assert!(
        c.message
            .contains(&format!("Used: {} (first match;", text(&a)))
    );
    assert!(c.message.contains(&format!(
        "the search order is {}, then {})",
        text(&a),
        text(&b)
    )));
    assert!(c.message.ends_with(&format!("Ignored: {}", text(&b))));
    let fix = c.fix.unwrap();
    assert!(fix.contains("--config PATH"));
}

#[test]
fn three_configs_ignore_all_but_the_first() {
    let (a, b, d) = (place("a"), place("b"), place("d"));
    let all = [a.clone(), b.clone(), d.clone()];
    let c = check::configs(&all, &all, None);
    assert_eq!(c.status, Status::Warn);
    assert!(c.message.starts_with("3 configs found."));
    assert!(
        c.message
            .ends_with(&format!("Ignored: {}, {}", text(&b), text(&d)))
    );
}

#[test]
fn an_explicit_config_with_another_one_around_is_ok_and_names_the_ignored_one() {
    let (a, b) = (place("a"), place("b"));
    let searched = [a.clone(), b.clone()];
    let c = check::configs(&searched, &searched, Some(b.as_path()));
    assert_eq!(c.status, Status::Ok);
    assert_eq!(
        c.message,
        format!(
            "{} (chosen with --config); also found but ignored: {}",
            text(&b),
            text(&a)
        )
    );
    assert_eq!(c.fix, None);
}

#[test]
fn an_explicit_config_alone_is_ok() {
    let (a, b) = (place("a"), place("b"));
    let c = check::configs(&[a.clone(), b], std::slice::from_ref(&a), Some(a.as_path()));
    assert_eq!(c.status, Status::Ok);
    assert_eq!(c.message, format!("{} (chosen with --config)", text(&a)));
}

#[test]
fn an_explicit_config_outside_the_search_paths_is_ok() {
    let (a, other) = (place("a"), place("elsewhere"));
    let c = check::configs(std::slice::from_ref(&a), &[], Some(other.as_path()));
    assert_eq!(c.status, Status::Ok);
    assert_eq!(
        c.message,
        format!("{} (chosen with --config)", text(&other))
    );
}
