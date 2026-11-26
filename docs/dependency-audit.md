# Dependency audit

The `audit` job in CI runs on pull requests and pushes to `main`. It installs cargo-audit 0.22.2 with the tool's locked dependencies and runs `cargo audit --file Cargo.lock` against RustSec's current advisory database. A vulnerability or a failed audit command fails the job. The job needs read access to repository contents. There is no ignore list.

## Reproduce the failure check

With cargo-audit installed, run the CI command from the repository root. It refreshes the advisory database. Then supply the following separate lockfile on standard input in PowerShell. No dependency is installed, compiled or added to the application.

```powershell
cargo audit --file Cargo.lock
```

```powershell
$fixture = @'
version = 3
[[package]]
name = "time"
version = "0.2.22"
source = "registry+https://github.com/rust-lang/crates.io-index"
'@
$report = $fixture | cargo audit --no-fetch --no-yanked --file - --json | ConvertFrom-Json
$auditExit = $LASTEXITCODE
if ($auditExit -ne 1 -or 'RUSTSEC-2020-0071' -notin $report.vulnerabilities.list.advisory.id) {
    throw "The fixture did not fail for the expected advisory"
}
```

The fixture uses the same advisory matcher as the PR job. It reuses the downloaded database and disables the unrelated yanked-package lookup to keep the failure reproducible offline.
