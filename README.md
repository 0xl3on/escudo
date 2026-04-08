# escudo

Supply chain checker for Rust. Catches fresh crates, banned dependencies, license violations, duplicate versions, and unverifiable packages before they hit production.

## Install

```
cargo install escudo
```

Upgrade to the latest version:

```
escudo upgrade
```

## Usage

```bash
# audit current project
escudo

# audit another project
escudo ~/path/to/project

# override config from the command line
escudo --cooldown-days 30
escudo --cooldown-days 14 --allow-git-deps

# combine
escudo ~/path/to/project --cooldown-days 30 --json

# policy checks only (no network, instant)
escudo --skip-freshness

# HTML report
escudo --html
```

Exit codes: `0` clean, `1` violations found, `2` runtime error.

## Defaults

Escudo works with zero config. Without an `escudo.toml` or CLI flags, it runs with:

| Check | Default | Behavior |
|-------|---------|----------|
| Freshness | `cooldown_days = 7` | Always on. Flags crates published less than 7 days ago. |
| Git deps | `allow_git_deps = false` | Always on. Flags git-sourced dependencies. |
| Path deps | `allow_path_deps = false` | Always on. Flags local path dependencies. |
| Banned crates | `banned_crates = []` | Skipped until configured. |
| Duplicate versions | `max_duplicate_versions` not set | Skipped until configured. |
| Licenses | `allowed_licenses = []` | Skipped until configured. |

If no config file is found and no CLI overrides are passed, escudo prints a notice explaining what's active and what's skipped.

## Config

Drop an `escudo.toml` in your project root to enable all checks. All fields are optional. CLI args override the config file.

```toml
cooldown_days = 7
allow_git_deps = false
allow_path_deps = false
max_duplicate_versions = 3
allowed_licenses = ["MIT", "Apache-2.0", "BSD-3-Clause", "ISC", "Unicode-3.0"]
banned_crates = []

# per-crate exceptions to the cooldown
[[exceptions]]
crate = "tokio"
reason = "tracking latest async runtime"
days = 3
```

Setting `max_duplicate_versions` or `allowed_licenses` in the config file enables those checks. Omitting them disables them.

### Exceptions

Exceptions override the global `cooldown_days` for specific crates. All three fields are required:

```toml
[[exceptions]]
crate = "fastrand"
reason = "transitive dep from iroh, out of our control"
days = 0

[[exceptions]]
crate = "tokio"
reason = "tracking latest async runtime"
days = 3
```

`days = 0` effectively disables the freshness check for that crate. If an exception references a crate that isn't in your dependency tree, escudo prints a warning to stderr so you can clean up stale entries.

## Checks

| Check | What it catches |
|-------|----------------|
| Unverified | Crate version not found in crates.io API response. Could indicate a yanked version, registry inconsistency, or compromised API. Always fails the audit. |
| Freshness | Crate version published within `cooldown_days`. Queries crates.io with a persistent disk cache (refetches only when a required version is missing). Future publish dates are rejected. |
| Git deps | Dependencies sourced from git repos (unless `allow_git_deps = true`). |
| Path deps | Local path dependencies (unless `allow_path_deps = true`). |
| Duplicates | Same crate with multiple versions exceeding `max_duplicate_versions`. Only checked when set. |
| Banned | Any crate listed in `banned_crates`. Only checked when non-empty. |
| Licenses | Crate license not in `allowed_licenses`. Handles SPDX expressions (`MIT OR Apache-2.0`, `Apache-2.0 AND ISC`). Only checked when non-empty. |

## CLI reference

```
escudo [OPTIONS] [PATH] [COMMAND]

Commands:
  upgrade    Update escudo to the latest version

Arguments:
  [PATH]     Path to the project to audit (default: current directory)

Options:
  --config <FILE>                  Path to escudo.toml
  --cooldown-days <N>              Override cooldown period
  --allow-git-deps                 Override: allow git dependencies
  --allow-path-deps                Override: allow path dependencies
  --max-duplicate-versions <N>     Override: max duplicate versions
  --json                           JSON output
  --html                           HTML report (opens in browser)
  --format=github                  GitHub Actions annotations (auto-detected in CI)
  --skip-freshness                 Skip crates.io lookups (policy checks only)
  --no-color                       Disable colors (also respects NO_COLOR env)
  -h, --help                       Print help
  -V, --version                    Print version
```

## CI

### GitHub Actions

```yaml
- name: Install escudo
  run: cargo install escudo

- name: Run supply chain check
  run: escudo
```

Annotations show up inline on PRs automatically (`GITHUB_ACTIONS` env is detected).

Policy checks only (no network, instant):

```yaml
- run: escudo --skip-freshness
```

### Caching

Persist the freshness cache between CI runs:

```yaml
- name: Cache escudo
  uses: actions/cache@v4
  with:
    path: /tmp/escudo-cache
    key: escudo-${{ hashFiles('Cargo.lock') }}

- name: Run escudo
  run: escudo
  env:
    ESCUDO_CACHE_DIR: /tmp/escudo-cache
```

### GitLab CI

```yaml
supply-chain:
  script:
    - cargo install escudo
    - escudo --no-color
```

### Pre-commit hook

```bash
#!/bin/sh
escudo --skip-freshness || exit 1
```

## Environment variables

| Variable | Effect |
|----------|--------|
| `ESCUDO_CACHE_DIR` | Override cache directory (default: `$XDG_CACHE_HOME/escudo`) |
| `NO_COLOR` | Disable colored output |
| `GITHUB_ACTIONS` | Auto-selects `--format=github` |
| `BROWSER=none` | Suppress auto-open for `--html` |

## Security model

Escudo queries the crates.io API (`/api/v1/crates/{name}/versions`) for publish dates during freshness checks. All other checks are fully local via `cargo metadata`.

Defenses against a compromised crates.io API:

- **Missing versions are violations** — if the API omits a version you depend on, the audit fails.
- **Future publish dates are rejected** — prevents an attacker from backdating a malicious crate.
- **Crate names are validated** — path traversal or injection via malicious crate names is blocked.
- **Request timeouts** — 15s connect, 30s per request. A hanging API cannot stall the audit.
- **Smart retries** — 429 and 5xx get exponential backoff. 4xx errors fail immediately.
- **Freshness errors are fatal** — if any crate cannot be verified, the audit fails. No silent skips.
- **Empty API responses are not cached** — prevents cache poisoning from anomalous responses.

## Library usage

Escudo can be used as a library. Add it to your `Cargo.toml`:

```toml
[dependencies]
escudo = "0.1.3"
```

```rust
use escudo::{check, CheckOptions};
use escudo::config::EscudoConfig;

#[tokio::main]
async fn main() -> Result<(), escudo::error::EscudoError> {
    let config = EscudoConfig::default();
    let opts = CheckOptions {
        manifest_path: None,     // current directory
        skip_freshness: false,
    };

    let report = check(&config, &opts).await?;

    println!("checked {} packages", report.packages_checked);
    if report.violations.is_empty() {
        println!("clean");
    } else {
        for v in &report.violations {
            println!("  {}", v);
        }
    }
    Ok(())
}
```

### Key types

| Type | Description |
|------|-------------|
| `EscudoConfig` | Deserialized `escudo.toml` — all fields optional with sensible defaults |
| `CheckOptions` | Runtime options: `manifest_path`, `skip_freshness` |
| `EscudoReport` | Result of an audit: `packages_checked` count + `violations` vec |
| `Violation` | Enum: `Freshness(FreshnessViolation)`, `Policy(PolicyViolation)`, `Unverified { crate_name, version }` |
| `EscudoError` | Top-level error: `Config`, `Metadata`, `Freshness`, `Report` variants |

## License

MIT OR Apache-2.0
