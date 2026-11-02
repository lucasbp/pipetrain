use std::fs;
use std::path::{Path, PathBuf};

use super::error::UpdateError;

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

fn failure(exe: &Path, error: impl ToString) -> UpdateError {
    UpdateError::Replace {
        path: exe.display().to_string(),
        reason: error.to_string(),
    }
}

pub fn discard_leftover(exe: &Path) {
    let _ = fs::remove_file(sibling(exe, ".old"));
    let _ = fs::remove_file(sibling(exe, ".new"));
}

#[cfg(unix)]
fn copy_mode(from: &Path, to: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(from).map_or(0o755, |meta| meta.permissions().mode() & 0o7777);
    let _ = fs::set_permissions(to, fs::Permissions::from_mode(mode | 0o111));
}

#[cfg(not(unix))]
fn copy_mode(_from: &Path, _to: &Path) {}

pub fn replace(exe: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let staged = sibling(exe, ".new");
    let old = sibling(exe, ".old");
    let _ = fs::remove_file(&staged);
    fs::write(&staged, bytes).map_err(|error| {
        let _ = fs::remove_file(&staged);
        failure(exe, error)
    })?;
    copy_mode(exe, &staged);
    if cfg!(windows) {
        let _ = fs::remove_file(&old);
        fs::rename(exe, &old).map_err(|error| {
            let _ = fs::remove_file(&staged);
            failure(exe, error)
        })?;
        if let Err(error) = fs::rename(&staged, exe) {
            let _ = fs::rename(&old, exe);
            let _ = fs::remove_file(&staged);
            return Err(failure(exe, error));
        }
    } else {
        fs::rename(&staged, exe).map_err(|error| {
            let _ = fs::remove_file(&staged);
            failure(exe, error)
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pipetrain-replace-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn swaps_the_contents_and_leaves_nothing_staged() {
        let dir = scratch();
        let exe = dir.join("pipetrain-test");
        fs::write(&exe, b"old").unwrap();
        replace(&exe, b"new").unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert!(!sibling(&exe, ".new").exists());
        discard_leftover(&exe);
        assert!(!sibling(&exe, ".old").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_executable_keeps_the_directory_clean() {
        let dir = scratch();
        let exe = dir.join("ghost");
        if cfg!(windows) {
            let error = replace(&exe, b"new").unwrap_err();
            assert!(matches!(error, UpdateError::Replace { .. }), "{error:?}");
            assert!(!sibling(&exe, ".new").exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let exe = dir.join("pipetrain-test");
        fs::write(&exe, b"old").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o750)).unwrap();
        replace(&exe, b"new").unwrap();
        assert_eq!(
            fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o750
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
