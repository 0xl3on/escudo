use std::fmt;

use crate::freshness::FreshnessViolation;
use crate::policy::PolicyViolation;

/// Top-level error type for the escudo library.
#[derive(Debug, thiserror::Error)]
pub enum EscudoError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("metadata error: {0}")]
    Metadata(#[from] MetadataError),

    #[error("freshness check error: {0}")]
    Freshness(#[from] FreshnessError),

    #[error("report error: {0}")]
    Report(#[from] ReportError),
}

/// Errors from config loading/parsing.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: String },

    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
}

/// Errors from cargo metadata resolution.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("failed to execute `cargo metadata`: {0}")]
    Exec(#[from] cargo_metadata::Error),
}

/// Errors from freshness/crates.io checks.
#[derive(Debug, thiserror::Error)]
pub enum FreshnessError {
    #[error("failed to build HTTP client: {0}")]
    HttpClient(reqwest::Error),

    #[error("HTTP request failed for crate `{crate_name}`: {source}")]
    HttpRequest {
        crate_name: String,
        source: reqwest::Error,
    },

    #[error("crates.io returned HTTP {status} for crate `{crate_name}`")]
    HttpStatus {
        crate_name: String,
        status: reqwest::StatusCode,
    },

    #[error("failed to parse crates.io response for `{crate_name}`: {source}")]
    ParseResponse {
        crate_name: String,
        source: reqwest::Error,
    },

    #[error("could not determine platform cache directory")]
    NoCacheDir,

    #[error("cache I/O error: {0}")]
    CacheIo(#[from] std::io::Error),

    #[error("cache parse error for {path}: {source}")]
    CacheParse {
        path: String,
        source: serde_json::Error,
    },

    #[error("freshness task panicked")]
    TaskPanicked,

    #[error("invalid crate name in dependency tree: `{0}`")]
    InvalidCrateName(String),
}

/// Errors from report generation.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTML template rendering failed: {0}")]
    Template(#[from] handlebars::RenderError),

    #[error("failed to write report file: {0}")]
    Io(#[from] std::io::Error),
}

/// A single violation found during the escudo audit.
#[derive(Debug, Clone)]
pub enum Violation {
    /// A crate was published too recently.
    Freshness(FreshnessViolation),
    /// A policy rule was broken.
    Policy(PolicyViolation),
    /// A crate version we depend on was not found in the crates.io API response.
    /// This could indicate a yanked version, a registry inconsistency, or a
    /// compromised API omitting versions to bypass freshness checks.
    Unverified { crate_name: String, version: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Freshness(v) => {
                write!(
                    f,
                    "crate `{}` v{} published only {} days ago (cooldown: {} days)",
                    v.crate_name, v.version, v.age_days, v.cooldown_days,
                )
            }
            Violation::Policy(v) => write!(f, "{}", v),
            Violation::Unverified {
                crate_name,
                version,
            } => {
                write!(
                    f,
                    "could not verify {} v{} — version not found in crates.io API response",
                    crate_name, version,
                )
            }
        }
    }
}
