pub mod config;
pub mod error;
pub mod freshness;
pub mod metadata;
pub mod policy;
pub mod report;

use crate::config::EscudoConfig;
use crate::error::{EscudoError, Violation};
use crate::freshness::check_freshness;
use crate::metadata::get_all_packages;
use crate::policy::check_policies;
use crate::report::EscudoReport;

/// Options that control how the audit runs (typically derived from CLI flags).
#[derive(Default)]
pub struct CheckOptions<'a> {
    /// Path to the workspace `Cargo.toml`. `None` = current directory.
    pub manifest_path: Option<&'a str>,
    /// When `true`, skip all network calls (freshness checks).
    pub skip_freshness: bool,
}

/// Run the full escudo supply-chain audit.
pub async fn check(
    config: &EscudoConfig,
    opts: &CheckOptions<'_>,
) -> Result<EscudoReport, EscudoError> {
    let packages = get_all_packages(opts.manifest_path)?;

    for exc in &config.exceptions {
        let matched = packages.iter().any(|p| p.name == exc.crate_name);
        if !matched {
            eprintln!(
                "warning: exception for `{}` does not match any dependency",
                exc.crate_name
            );
        }
    }

    let mut violations = Vec::new();

    if !opts.skip_freshness {
        let result = check_freshness(config, &packages).await?;
        for fv in result.violations {
            violations.push(Violation::Freshness(fv));
        }
        for uv in result.unverified {
            violations.push(Violation::Unverified {
                crate_name: uv.crate_name,
                version: uv.version,
            });
        }
    }

    let policy_violations = check_policies(config, &packages);
    for pv in policy_violations {
        violations.push(Violation::Policy(pv));
    }

    Ok(EscudoReport {
        violations,
        packages_checked: packages.len(),
    })
}
