use chrono::NaiveDate;
use pipetrain::command::parse_date;
use pipetrain::config::Config;
use pipetrain::discovery::train_branch;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 29).unwrap()
}

fn error_text(toml: &str) -> String {
    format!("{:#}", Config::parse(toml).unwrap_err())
}

#[test]
fn default_format_is_day_month_year() {
    let cfg = Config::default();
    assert_eq!(cfg.train_date_format, "%d-%m-%Y");
    assert_eq!(train_branch(&cfg, day()), "deploy/29-10-2026");
}

#[test]
fn config_without_the_key_keeps_the_default() {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    assert_eq!(train_branch(&cfg, day()), "deploy/29-10-2026");
}

#[test]
fn iso_format_names_the_branch() {
    let cfg = Config::parse("train_date_format = \"%Y-%m-%d\"").unwrap();
    assert_eq!(train_branch(&cfg, day()), "deploy/2026-10-29");
}

#[test]
fn prefix_and_format_combine() {
    let cfg = Config::parse("train_prefix = \"release/\"\ntrain_date_format = \"%Y%m%d\"").unwrap();
    assert_eq!(train_branch(&cfg, day()), "release/20261029");
}

#[test]
fn invalid_format_is_rejected_naming_the_key() {
    let text = error_text("train_date_format = \"%Q-%d\"");
    assert!(text.contains("train_date_format"), "{text}");
    assert!(text.contains("%Q-%d"), "{text}");
}

#[test]
fn dangling_percent_is_rejected() {
    assert!(error_text("train_date_format = \"%d-%\"").contains("train_date_format"));
}

#[test]
fn empty_format_is_rejected() {
    let text = error_text("train_date_format = \"\"");
    assert!(text.contains("train_date_format"), "{text}");
    assert!(text.contains("empty"), "{text}");
}

#[test]
fn format_without_a_day_is_rejected() {
    for format in ["%Y-%m", "%Y", "static"] {
        let text = error_text(&format!("train_date_format = \"{format}\""));
        assert!(text.contains("train_date_format"), "{text}");
        assert!(text.contains("day"), "{text}");
    }
}

#[test]
fn format_that_cannot_format_a_date_is_rejected_without_panic() {
    for format in ["%d-%H:%M", "%d %Z"] {
        let text = error_text(&format!("train_date_format = \"{format}\""));
        assert!(text.contains("train_date_format"), "{text}");
    }
}

#[test]
fn date_flag_accepts_the_default_format() {
    assert_eq!(parse_date("29-10-2026", "%d-%m-%Y").unwrap(), day());
}

#[test]
fn date_flag_accepts_the_configured_format_and_iso() {
    assert_eq!(parse_date("29/10/26", "%d/%m/%y").unwrap(), day());
    assert_eq!(parse_date("2026-10-29", "%d/%m/%y").unwrap(), day());
    assert_eq!(parse_date("20261029", "%Y%m%d").unwrap(), day());
    assert_eq!(parse_date("2026-10-29", "%Y%m%d").unwrap(), day());
}

#[test]
fn date_flag_error_lists_what_is_accepted() {
    let text = format!("{:#}", parse_date("tomorrow", "%d-%m-%Y").unwrap_err());
    assert!(text.contains("29-10-2026"), "{text}");
    assert!(text.contains("2026-10-29"), "{text}");
    assert!(text.contains("train_date_format"), "{text}");
    assert!(text.contains("tomorrow"), "{text}");
}

#[test]
fn date_flag_error_with_iso_configured_does_not_repeat_itself() {
    let text = format!("{:#}", parse_date("tomorrow", "%Y-%m-%d").unwrap_err());
    assert!(text.contains("2026-10-29"), "{text}");
    assert!(!text.contains("train_date_format"), "{text}");
}
