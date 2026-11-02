mod record;
pub mod stats;
mod store;
pub mod summary;

use std::path::{Path, PathBuf};

pub use record::{ErrorKind, Record, build, kind_of};
pub use store::{FILES, MAX_BYTES, append, read_all};

use crate::profile::ProfileSet;

pub const ENV_ON: &str = "PIPETRAIN_LOG";
pub const ENV_DIR: &str = "PIPETRAIN_LOG_DIR";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    On,
    Off,
    Unset,
}

pub fn parse_env(value: Option<&str>) -> Env {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return Env::Unset;
    };
    if text == "0" || text.eq_ignore_ascii_case("false") {
        Env::Off
    } else {
        Env::On
    }
}

pub fn decide(no_log: bool, env: Env, config: impl FnOnce() -> bool) -> bool {
    if no_log {
        return false;
    }
    match env {
        Env::Off => false,
        Env::On => true,
        Env::Unset => config(),
    }
}

pub fn enabled(no_log: bool, config: Option<&Path>) -> bool {
    decide(
        no_log,
        parse_env(std::env::var(ENV_ON).ok().as_deref()),
        || config_says_on(config),
    )
}

fn config_says_on(config: Option<&Path>) -> bool {
    ProfileSet::load(config)
        .ok()
        .and_then(|(set, _)| set.profiles.into_iter().next())
        .is_some_and(|profile| profile.config.log)
}

pub fn resolve_dir(env: Option<String>, data_dir: Option<PathBuf>) -> Option<PathBuf> {
    match env {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => data_dir.map(|dir| dir.join("pipetrain")),
    }
}

pub fn default_dir() -> Option<PathBuf> {
    resolve_dir(std::env::var(ENV_DIR).ok(), dirs::data_dir())
}

pub fn write(record: &Record) {
    if let Some(dir) = default_dir() {
        append(&dir, record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_reads_zero_and_false_as_off() {
        assert_eq!(parse_env(Some("0")), Env::Off);
        assert_eq!(parse_env(Some(" FALSE ")), Env::Off);
        assert_eq!(parse_env(Some("1")), Env::On);
        assert_eq!(parse_env(Some("yes")), Env::On);
        assert_eq!(parse_env(Some("")), Env::Unset);
        assert_eq!(parse_env(None), Env::Unset);
    }

    #[test]
    fn off_wins_over_everything_and_the_config_is_the_last_word() {
        assert!(!decide(true, Env::On, || true));
        assert!(!decide(false, Env::Off, || true));
        assert!(decide(false, Env::On, || false));
        assert!(decide(false, Env::Unset, || true));
        assert!(!decide(false, Env::Unset, || false));
    }

    #[test]
    fn the_config_is_not_read_when_the_env_decides() {
        assert!(decide(false, Env::On, || panic!("must not read")));
        assert!(!decide(false, Env::Off, || panic!("must not read")));
        assert!(!decide(true, Env::Unset, || panic!("must not read")));
    }

    #[test]
    fn the_dir_comes_from_the_env_or_the_data_dir() {
        assert_eq!(
            resolve_dir(Some("/tmp/x".into()), Some("/data".into())),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            resolve_dir(None, Some(PathBuf::from("/data"))),
            Some(PathBuf::from("/data").join("pipetrain"))
        );
        assert_eq!(
            resolve_dir(Some(String::new()), Some(PathBuf::from("/data"))),
            Some(PathBuf::from("/data").join("pipetrain"))
        );
        assert_eq!(resolve_dir(None, None), None);
    }
}
