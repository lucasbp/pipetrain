use anyhow::Result;

use crate::log::{self, summary};

pub struct LogOptions {
    pub summary: bool,
    pub json: bool,
}

pub fn run(opts: LogOptions) -> Result<i32> {
    if !opts.summary {
        println!(
            "pipetrain log --summary [--json]: counts, error rates and median time from the local log"
        );
        return Ok(0);
    }
    let records = log::default_dir()
        .map(|dir| log::read_all(&dir))
        .unwrap_or_default();
    if records.is_empty() {
        eprintln!(
            "no log yet: it is off by default. Turn it on with `log = true` in the config or PIPETRAIN_LOG=1; see docs/log.md"
        );
    }
    let built = summary::build(&records);
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&summary::json(&built))?);
    } else {
        print!("{}", summary::text(&built));
    }
    Ok(0)
}
