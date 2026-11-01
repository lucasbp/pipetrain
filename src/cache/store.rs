use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const VERSION: u64 = 1;
const MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Deserialize)]
struct Stored {
    v: u64,
    data: Value,
}

#[derive(Serialize)]
struct Entry<'a> {
    v: u64,
    data: &'a Value,
}

pub(super) fn read(path: &Path) -> Option<Value> {
    let len = fs::metadata(path).ok()?.len();
    if len > MAX_ENTRY_BYTES as u64 {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let stored: Stored = serde_json::from_slice(&bytes).ok()?;
    (stored.v == VERSION).then_some(stored.data)
}

pub(super) fn write(path: &Path, data: &Value) {
    let entry = Entry { v: VERSION, data };
    let Ok(bytes) = serde_json::to_vec(&entry) else {
        return;
    };
    if bytes.len() > MAX_ENTRY_BYTES {
        return;
    }
    let _ = write_atomic(path, &bytes);
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("entry path has no parent"))?;
    fs::create_dir_all(parent)?;
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".tmp-{}-{n}", std::process::id()));
    let result = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
