pub(crate) mod manifests;

use crate::feed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Change,
    UpToDate,
    Ahead,
    Range,
    OtherLevel,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::UpToDate => "up_to_date",
            Self::Ahead => "ahead",
            Self::Range => "range",
            Self::OtherLevel => "other_level",
        }
    }
}

fn is_exact(spec: &str) -> bool {
    let core = spec.split(['-', '+']).next().unwrap_or_default();
    !core.is_empty()
        && core
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && spec
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

pub fn classify(current: &str, target: &str) -> Status {
    if !is_exact(current) {
        return Status::Range;
    }
    match feed::compare(current, target) {
        std::cmp::Ordering::Equal => Status::UpToDate,
        std::cmp::Ordering::Less => Status::Change,
        std::cmp::Ordering::Greater => Status::Ahead,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Level {
    Major,
    Minor,
    Patch,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Patch => "patch",
        }
    }
}

pub fn level_of(current: &str, target: &str) -> Level {
    let from = feed::Version::parse(current);
    let to = feed::Version::parse(target);
    let (from, to) = (from.numbers(), to.numbers());
    for index in 0..from.len().max(to.len()) {
        if from.get(index).unwrap_or(&0) != to.get(index).unwrap_or(&0) {
            return match index {
                0 => Level::Major,
                1 => Level::Minor,
                _ => Level::Patch,
            };
        }
    }
    Level::Patch
}

pub(super) fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

pub(super) fn sibling(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}
