use serde::Deserialize;
use std::path::Path;

use crate::error::ConfigError;

/// Per-crate exception that relaxes the freshness cooldown.
#[derive(Debug, Clone, Deserialize)]
pub struct CrateException {
    /// The crate name this exception applies to.
    #[serde(rename = "crate")]
    pub crate_name: String,
    /// Human-readable reason for the exception.
    pub reason: String,
    /// Custom cooldown in days for this crate.
    pub days: u32,
}

/// Top-level escudo configuration, typically loaded from `escudo.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EscudoConfig {
    /// Minimum number of days since the last publish before a crate is
    /// considered "settled". Newer publishes trigger a warning.
    pub cooldown_days: u32,
    /// Whether to allow git-source dependencies.
    pub allow_git_deps: bool,
    /// Whether to allow path-source dependencies.
    pub allow_path_deps: bool,
    /// Maximum number of different versions of the same crate allowed in the
    /// dependency tree before a warning is emitted. `None` = skip this check.
    pub max_duplicate_versions: Option<u32>,
    /// SPDX license identifiers that are considered acceptable.
    pub allowed_licenses: Vec<String>,
    /// Crate names that should always be flagged, regardless of other checks.
    pub banned_crates: Vec<String>,
    /// Per-crate exceptions for the freshness cooldown.
    pub exceptions: Vec<CrateException>,
}

impl Default for EscudoConfig {
    fn default() -> Self {
        Self {
            cooldown_days: 7,
            allow_git_deps: false,
            allow_path_deps: false,
            max_duplicate_versions: None,
            allowed_licenses: Vec::new(),
            banned_crates: Vec::new(),
            exceptions: Vec::new(),
        }
    }
}

/// Load an [`EscudoConfig`] from disk.
///
/// - If `path` is `Some`, read that file.
/// - Otherwise look for `escudo.toml` in the current directory.
/// - If neither exists, return the built-in defaults.
pub fn load_config(path: Option<&str>) -> Result<EscudoConfig, ConfigError> {
    let config_path = match path {
        Some(p) => {
            let p = Path::new(p);
            if p.exists() {
                Some(p.to_path_buf())
            } else {
                return Err(ConfigError::NotFound {
                    path: p.display().to_string(),
                });
            }
        }
        None => {
            let default_path = Path::new("escudo.toml");
            if default_path.exists() {
                Some(default_path.to_path_buf())
            } else {
                None
            }
        }
    };

    match config_path {
        Some(p) => {
            let contents = std::fs::read_to_string(&p)?;
            let config: EscudoConfig = toml::from_str(&contents)?;
            Ok(config)
        }
        None => Ok(EscudoConfig::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let cfg = EscudoConfig::default();
        assert_eq!(cfg.cooldown_days, 7);
        assert!(!cfg.allow_git_deps);
        assert!(!cfg.allow_path_deps);
        assert_eq!(cfg.max_duplicate_versions, None);
        assert!(cfg.allowed_licenses.is_empty());
        assert!(cfg.banned_crates.is_empty());
        assert!(cfg.exceptions.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
cooldown_days = 14
allow_git_deps = true
allow_path_deps = true
max_duplicate_versions = 2
allowed_licenses = ["MIT"]
banned_crates = ["evil-crate"]

[[exceptions]]
crate = "tokio"
reason = "needed for latest async features"
days = 3
"#;
        let cfg: EscudoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.cooldown_days, 14);
        assert!(cfg.allow_git_deps);
        assert!(cfg.allow_path_deps);
        assert_eq!(cfg.max_duplicate_versions, Some(2));
        assert_eq!(cfg.allowed_licenses, vec!["MIT"]);
        assert_eq!(cfg.banned_crates, vec!["evil-crate"]);
        assert_eq!(cfg.exceptions.len(), 1);
        assert_eq!(cfg.exceptions[0].crate_name, "tokio");
        assert_eq!(cfg.exceptions[0].reason, "needed for latest async features");
        assert_eq!(cfg.exceptions[0].days, 3);
    }

    #[test]
    fn test_parse_partial_config() {
        let toml_str = r#"
cooldown_days = 30
"#;
        let cfg: EscudoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.cooldown_days, 30);
        // Everything else falls back to defaults.
        assert!(!cfg.allow_git_deps);
        assert_eq!(cfg.max_duplicate_versions, None);
        assert!(cfg.allowed_licenses.is_empty());
    }

    #[test]
    fn test_parse_empty_config() {
        let cfg: EscudoConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.cooldown_days, 7);
        assert!(!cfg.allow_git_deps);
    }

    #[test]
    fn test_load_config_no_file_returns_defaults() {
        // When no path is given and escudo.toml doesn't exist in cwd,
        // we should get defaults.
        let cfg = load_config(None).unwrap();
        assert_eq!(cfg.cooldown_days, 7);
    }

    #[test]
    fn test_load_config_missing_explicit_path_errors() {
        let result = load_config(Some("/tmp/nonexistent_escudo_config_12345.toml"));
        assert!(result.is_err());
    }
}
