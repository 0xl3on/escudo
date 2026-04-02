use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::EscudoConfig;
use crate::metadata::{PackageInfo, PackageSource, find_duplicate_versions};

/// A single policy violation discovered during the audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PolicyViolation {
    /// A dependency is sourced from a git repository, but git deps are not allowed.
    GitDependency { name: String, url: String },
    /// A dependency is sourced from a local path, but path deps are not allowed.
    PathDependency { name: String, path: String },
    /// More versions of a single crate exist in the tree than the policy permits.
    DuplicateVersions {
        name: String,
        versions: Vec<String>,
        max_allowed: u32,
    },
    /// A dependency matches an entry on the explicit ban list.
    BannedCrate { name: String, version: String },
    /// A dependency's license is missing or not in the set of allowed licenses.
    DisallowedLicense {
        name: String,
        version: String,
        license: Option<String>,
    },
}

impl fmt::Display for PolicyViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyViolation::GitDependency { name, url } => {
                write!(f, "git dependency not allowed: {} (from {})", name, url)
            }
            PolicyViolation::PathDependency { name, path } => {
                write!(f, "path dependency not allowed: {} (at {})", name, path)
            }
            PolicyViolation::DuplicateVersions {
                name,
                versions,
                max_allowed,
            } => {
                write!(
                    f,
                    "too many versions of {}: found {} ({}) but at most {} allowed",
                    name,
                    versions.len(),
                    versions.join(", "),
                    max_allowed,
                )
            }
            PolicyViolation::BannedCrate { name, version } => {
                write!(f, "banned crate: {} v{}", name, version)
            }
            PolicyViolation::DisallowedLicense {
                name,
                version,
                license,
            } => match license {
                Some(lic) => write!(
                    f,
                    "license not allowed for {} v{}: \"{}\"",
                    name, version, lic
                ),
                None => write!(f, "no license specified for {} v{}", name, version),
            },
        }
    }
}

/// Run every configured policy check against the resolved package list and
/// return all violations found.
pub fn check_policies(config: &EscudoConfig, packages: &[PackageInfo]) -> Vec<PolicyViolation> {
    let mut violations = Vec::new();

    check_git_deps(config, packages, &mut violations);
    check_path_deps(config, packages, &mut violations);
    check_duplicates(config, packages, &mut violations);
    check_banned(config, packages, &mut violations);
    check_licenses(config, packages, &mut violations);

    violations
}

/// Flag any git-sourced dependency when `allow_git_deps` is false.
fn check_git_deps(
    config: &EscudoConfig,
    packages: &[PackageInfo],
    violations: &mut Vec<PolicyViolation>,
) {
    if config.allow_git_deps {
        return;
    }
    for pkg in packages {
        if let PackageSource::Git { url } = &pkg.source {
            violations.push(PolicyViolation::GitDependency {
                name: pkg.name.clone(),
                url: url.clone(),
            });
        }
    }
}

/// Flag any path-sourced dependency when `allow_path_deps` is false.
fn check_path_deps(
    config: &EscudoConfig,
    packages: &[PackageInfo],
    violations: &mut Vec<PolicyViolation>,
) {
    if config.allow_path_deps {
        return;
    }
    for pkg in packages {
        if let PackageSource::Path { path } = &pkg.source {
            violations.push(PolicyViolation::PathDependency {
                name: pkg.name.clone(),
                path: path.clone(),
            });
        }
    }
}

/// Flag crate names that appear with more distinct versions than the policy
/// allows.
fn check_duplicates(
    config: &EscudoConfig,
    packages: &[PackageInfo],
    violations: &mut Vec<PolicyViolation>,
) {
    let Some(max) = config.max_duplicate_versions else {
        return;
    };
    let dupes = find_duplicate_versions(packages);
    for (name, versions) in dupes {
        if versions.len() as u32 > max {
            violations.push(PolicyViolation::DuplicateVersions {
                name,
                versions,
                max_allowed: max,
            });
        }
    }
}

/// Flag any package whose name appears on the ban list.
fn check_banned(
    config: &EscudoConfig,
    packages: &[PackageInfo],
    violations: &mut Vec<PolicyViolation>,
) {
    if config.banned_crates.is_empty() {
        return;
    }
    for pkg in packages {
        if config.banned_crates.contains(&pkg.name) {
            violations.push(PolicyViolation::BannedCrate {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
        }
    }
}

/// Check that every crates.io package carries a license present in the allowed
/// set.  License expressions using "/" or "OR" as separators are split so that
/// compound expressions like "MIT OR Apache-2.0" pass when either component is
/// allowed.
fn check_licenses(
    config: &EscudoConfig,
    packages: &[PackageInfo],
    violations: &mut Vec<PolicyViolation>,
) {
    // An empty allowed list means "don't check licenses".
    if config.allowed_licenses.is_empty() {
        return;
    }

    for pkg in packages {
        // Only enforce license policy on crates.io packages.
        if !matches!(pkg.source, PackageSource::CratesIo) {
            continue;
        }

        let is_acceptable = match &pkg.license {
            Some(license_expr) => license_is_acceptable(license_expr, &config.allowed_licenses),
            None => false,
        };

        if !is_acceptable {
            violations.push(PolicyViolation::DisallowedLicense {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                license: pkg.license.clone(),
            });
        }
    }
}

/// Determine whether a license expression satisfies the allowed set.
///
/// Handles SPDX compound expressions:
///   - `OR` / `/` — disjunction: at least one alternative must be allowed
///   - `AND` — conjunction: every component must be allowed
///   - Parentheses are stripped for simplicity
///
/// The expression is accepted if, after splitting on `AND`, every conjunct
/// contains at least one `OR`-alternative that appears in the allowed list.
fn license_is_acceptable(license_expr: &str, allowed: &[String]) -> bool {
    // Strip parentheses — we don't need full SPDX parse trees for the common
    // patterns found in the Cargo ecosystem.
    let cleaned = license_expr.replace(['(', ')'], "");

    // Split on " AND " first; every conjunct must independently pass.
    let conjuncts = cleaned.split(" AND ");

    for conjunct in conjuncts {
        // Within a conjunct, split on " OR " and "/" — any alternative suffices.
        let has_match = conjunct
            .split(" OR ")
            .flat_map(|part| part.split('/'))
            .map(|s| s.trim())
            .any(|id| allowed.iter().any(|a| a == id));

        if !has_match {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> EscudoConfig {
        EscudoConfig::default()
    }

    fn cratesio_pkg(name: &str, version: &str, license: Option<&str>) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            source: PackageSource::CratesIo,
            license: license.map(|s| s.to_string()),
            manifest_path: String::new(),
        }
    }

    fn git_pkg(name: &str, url: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            source: PackageSource::Git {
                url: url.to_string(),
            },
            license: Some("MIT".to_string()),
            manifest_path: String::new(),
        }
    }

    fn path_pkg(name: &str, path: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            source: PackageSource::Path {
                path: path.to_string(),
            },
            license: Some("MIT".to_string()),
            manifest_path: String::new(),
        }
    }

    #[test]
    fn git_dep_flagged_when_disallowed() {
        let config = default_config(); // allow_git_deps = false
        let packages = vec![git_pkg("my-crate", "https://github.com/foo/bar")];
        let v = check_policies(&config, &packages);
        assert_eq!(v.len(), 1);
        assert!(matches!(&v[0], PolicyViolation::GitDependency { name, .. } if name == "my-crate"));
    }

    #[test]
    fn git_dep_allowed_when_configured() {
        let mut config = default_config();
        config.allow_git_deps = true;
        let packages = vec![git_pkg("my-crate", "https://github.com/foo/bar")];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn path_dep_flagged_when_disallowed() {
        let config = default_config(); // allow_path_deps = false
        let packages = vec![path_pkg("local-lib", "../local-lib")];
        let v = check_policies(&config, &packages);
        assert_eq!(v.len(), 1);
        assert!(
            matches!(&v[0], PolicyViolation::PathDependency { name, .. } if name == "local-lib")
        );
    }

    #[test]
    fn path_dep_allowed_when_configured() {
        let mut config = default_config();
        config.allow_path_deps = true;
        let packages = vec![path_pkg("local-lib", "../local-lib")];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn banned_crate_flagged() {
        let mut config = default_config();
        config.banned_crates = vec!["evil-crate".to_string()];
        let packages = vec![cratesio_pkg("evil-crate", "1.0.0", Some("MIT"))];
        let v = check_policies(&config, &packages);
        assert_eq!(v.len(), 1);
        assert!(matches!(&v[0], PolicyViolation::BannedCrate { name, .. } if name == "evil-crate"));
    }

    #[test]
    fn non_banned_crate_passes() {
        let mut config = default_config();
        config.banned_crates = vec!["evil-crate".to_string()];
        let packages = vec![cratesio_pkg("good-crate", "1.0.0", Some("MIT"))];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn allowed_license_passes() {
        let mut config = default_config();
        config.allowed_licenses = vec!["MIT".into(), "Apache-2.0".into()];
        let packages = vec![cratesio_pkg("serde", "1.0.0", Some("MIT OR Apache-2.0"))];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn disallowed_license_flagged() {
        let mut config = default_config();
        config.allowed_licenses = vec!["MIT".into()];
        let packages = vec![cratesio_pkg("gpl-crate", "1.0.0", Some("GPL-3.0"))];
        let v = check_policies(&config, &packages);
        assert_eq!(v.len(), 1);
        assert!(matches!(
            &v[0],
            PolicyViolation::DisallowedLicense { name, license, .. }
            if name == "gpl-crate" && license.as_deref() == Some("GPL-3.0")
        ));
    }

    #[test]
    fn missing_license_flagged() {
        let mut config = default_config();
        config.allowed_licenses = vec!["MIT".into()];
        let packages = vec![cratesio_pkg("no-license", "1.0.0", None)];
        let v = check_policies(&config, &packages);
        assert_eq!(v.len(), 1);
        assert!(matches!(
            &v[0],
            PolicyViolation::DisallowedLicense { license, .. } if license.is_none()
        ));
    }

    #[test]
    fn slash_license_expression_handled() {
        let mut config = default_config();
        config.allowed_licenses = vec!["MIT".into(), "Apache-2.0".into()];
        let packages = vec![cratesio_pkg("dual", "1.0.0", Some("MIT/Apache-2.0"))];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn empty_allowed_licenses_skips_check() {
        let mut config = default_config();
        config.allowed_licenses = Vec::new();
        let packages = vec![cratesio_pkg("anything", "1.0.0", Some("WTFPL"))];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn license_check_only_applies_to_cratesio() {
        let config = default_config();
        // git package with disallowed license -- should NOT be flagged by the
        // license check (git dep check is separate).
        let mut config_git_ok = config.clone();
        config_git_ok.allow_git_deps = true;
        let packages = vec![PackageInfo {
            name: "git-lib".to_string(),
            version: "0.1.0".to_string(),
            source: PackageSource::Git {
                url: "https://example.com".to_string(),
            },
            license: Some("GPL-3.0".to_string()),
            manifest_path: String::new(),
        }];
        let v = check_policies(&config_git_ok, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn duplicates_exceeding_max_flagged() {
        let mut config = default_config();
        config.max_duplicate_versions = Some(1);
        let packages = vec![
            cratesio_pkg("syn", "1.0.109", Some("MIT")),
            cratesio_pkg("syn", "2.0.50", Some("MIT")),
        ];
        let v = check_policies(&config, &packages);
        assert_eq!(v.len(), 1);
        assert!(matches!(
            &v[0],
            PolicyViolation::DuplicateVersions { name, versions, max_allowed }
            if name == "syn" && versions.len() == 2 && *max_allowed == 1
        ));
    }

    #[test]
    fn duplicates_within_limit_pass() {
        let mut config = default_config();
        config.max_duplicate_versions = Some(2);
        let packages = vec![
            cratesio_pkg("syn", "1.0.109", Some("MIT")),
            cratesio_pkg("syn", "2.0.50", Some("MIT")),
        ];
        let v = check_policies(&config, &packages);
        assert!(v.is_empty());
    }

    #[test]
    fn display_git_dependency() {
        let v = PolicyViolation::GitDependency {
            name: "foo".to_string(),
            url: "https://github.com/foo/bar".to_string(),
        };
        assert_eq!(
            v.to_string(),
            "git dependency not allowed: foo (from https://github.com/foo/bar)"
        );
    }

    #[test]
    fn display_path_dependency() {
        let v = PolicyViolation::PathDependency {
            name: "bar".to_string(),
            path: "../bar".to_string(),
        };
        assert_eq!(
            v.to_string(),
            "path dependency not allowed: bar (at ../bar)"
        );
    }

    #[test]
    fn display_duplicate_versions() {
        let v = PolicyViolation::DuplicateVersions {
            name: "syn".to_string(),
            versions: vec!["1.0.109".to_string(), "2.0.50".to_string()],
            max_allowed: 1,
        };
        assert_eq!(
            v.to_string(),
            "too many versions of syn: found 2 (1.0.109, 2.0.50) but at most 1 allowed"
        );
    }

    #[test]
    fn display_banned_crate() {
        let v = PolicyViolation::BannedCrate {
            name: "evil".to_string(),
            version: "0.1.0".to_string(),
        };
        assert_eq!(v.to_string(), "banned crate: evil v0.1.0");
    }

    #[test]
    fn display_disallowed_license_some() {
        let v = PolicyViolation::DisallowedLicense {
            name: "gpl-lib".to_string(),
            version: "3.0.0".to_string(),
            license: Some("GPL-3.0".to_string()),
        };
        assert_eq!(
            v.to_string(),
            "license not allowed for gpl-lib v3.0.0: \"GPL-3.0\""
        );
    }

    #[test]
    fn display_disallowed_license_none() {
        let v = PolicyViolation::DisallowedLicense {
            name: "mystery".to_string(),
            version: "1.0.0".to_string(),
            license: None,
        };
        assert_eq!(v.to_string(), "no license specified for mystery v1.0.0");
    }

    #[test]
    fn accept_simple_license() {
        assert!(license_is_acceptable("MIT", &["MIT".to_string()]));
    }

    #[test]
    fn reject_simple_license() {
        assert!(!license_is_acceptable("GPL-3.0", &["MIT".to_string()]));
    }

    #[test]
    fn accept_or_expression() {
        let allowed = vec!["Apache-2.0".to_string()];
        assert!(license_is_acceptable("MIT OR Apache-2.0", &allowed));
    }

    #[test]
    fn accept_slash_expression() {
        let allowed = vec!["Apache-2.0".to_string()];
        assert!(license_is_acceptable("MIT/Apache-2.0", &allowed));
    }

    #[test]
    fn accept_mixed_or_and_slash() {
        let allowed = vec!["ISC".to_string()];
        assert!(license_is_acceptable("MIT OR BSD-3-Clause/ISC", &allowed));
    }

    #[test]
    fn reject_when_no_component_matches() {
        let allowed = vec!["MIT".to_string()];
        assert!(!license_is_acceptable("GPL-3.0 OR LGPL-2.1", &allowed));
    }

    #[test]
    fn multiple_violations_collected() {
        let mut config = default_config();
        config.banned_crates = vec!["evil".to_string()];
        config.allowed_licenses = vec!["MIT".into()];

        let packages = vec![
            git_pkg("git-dep", "https://example.com"),
            path_pkg("local-dep", "/tmp/lib"),
            cratesio_pkg("evil", "0.1.0", Some("MIT")),
            cratesio_pkg("no-lic", "1.0.0", None),
        ];

        let v = check_policies(&config, &packages);
        // Expect: GitDependency, PathDependency, BannedCrate, DisallowedLicense
        assert_eq!(v.len(), 4);
    }
}
