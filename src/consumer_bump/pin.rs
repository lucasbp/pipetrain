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
