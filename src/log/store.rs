use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::record::Record;

pub const FILES: usize = 3;
pub const MAX_BYTES: u64 = 1024 * 1024;
const NAME: &str = "log";

fn file(dir: &Path, index: usize) -> PathBuf {
    if index == 0 {
        dir.join(format!("{NAME}.jsonl"))
    } else {
        dir.join(format!("{NAME}.{index}.jsonl"))
    }
}

fn rotate(dir: &Path) {
    let _ = fs::remove_file(file(dir, FILES - 1));
    for index in (0..FILES - 1).rev() {
        let from = file(dir, index);
        if from.is_file() {
            let _ = fs::rename(from, file(dir, index + 1));
        }
    }
}

fn append_line(dir: &Path, line: &str, max_bytes: u64) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let current = file(dir, 0);
    let size = fs::metadata(&current).map_or(0, |meta| meta.len());
    if size > 0 && size + line.len() as u64 > max_bytes {
        rotate(dir);
    }
    let mut out = OpenOptions::new().create(true).append(true).open(current)?;
    out.write_all(line.as_bytes())
}

pub fn append_with(dir: &Path, record: &Record, max_bytes: u64) {
    let Ok(mut line) = serde_json::to_string(record) else {
        return;
    };
    line.push('\n');
    let _ = append_line(dir, &line, max_bytes);
}

pub fn append(dir: &Path, record: &Record) {
    append_with(dir, record, MAX_BYTES);
}

pub fn read_all(dir: &Path) -> Vec<Record> {
    let mut records = Vec::new();
    for index in (0..FILES).rev() {
        let Ok(text) = fs::read_to_string(file(dir, index)) else {
            continue;
        };
        records.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Record>(line).ok()),
        );
    }
    records
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::log::stats::Counts;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "pipetrain-log-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn sample(command: &str) -> Record {
        super::super::record::build(command, 0, None, 5, Counts::default())
    }

    #[test]
    fn a_record_is_one_json_line() {
        let dir = scratch();
        append(&dir, &sample("plan"));
        append(&dir, &sample("doctor"));
        let text = fs::read_to_string(file(&dir, 0)).unwrap();
        assert_eq!(text.lines().count(), 2);
        let commands: Vec<String> = read_all(&dir).into_iter().map(|r| r.command).collect();
        assert_eq!(commands, ["plan", "doctor"]);
    }

    #[test]
    fn the_file_rotates_by_size_and_keeps_three_files() {
        let dir = scratch();
        for round in 0..40 {
            append_with(&dir, &sample(&format!("c{round}")), 400);
        }
        assert!(file(&dir, 0).is_file());
        assert!(file(&dir, 1).is_file());
        assert!(file(&dir, 2).is_file());
        assert!(!file(&dir, 3).exists());
        for index in 0..FILES {
            assert!(fs::metadata(file(&dir, index)).unwrap().len() <= 400);
        }
        let commands: Vec<String> = read_all(&dir).into_iter().map(|r| r.command).collect();
        assert_eq!(commands.last().map(String::as_str), Some("c39"));
    }

    #[test]
    fn a_dir_that_cannot_be_created_fails_silently() {
        let dir = scratch();
        fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        append(&blocker.join("inner"), &sample("plan"));
        assert!(read_all(&blocker.join("inner")).is_empty());
    }

    #[test]
    fn unreadable_lines_are_skipped() {
        let dir = scratch();
        fs::create_dir_all(&dir).unwrap();
        fs::write(file(&dir, 0), "{broken\n\n").unwrap();
        append(&dir, &sample("plan"));
        assert_eq!(read_all(&dir).len(), 1);
    }
}
