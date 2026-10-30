use std::process::Command;

use anyhow::{Result, bail};

pub fn token() -> Result<String> {
    if let Ok(out) = Command::new("gh").args(["auth", "token"]).output()
        && out.status.success()
    {
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }
    match std::env::var("GH_TOKEN") {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => bail!("no GitHub token: run `gh auth login`, or set GH_TOKEN"),
    }
}
