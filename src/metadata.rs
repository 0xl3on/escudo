use std::collections::HashMap;

use cargo_metadata::MetadataCommand;
use serde::{Deserialize, Serialize};

use crate::error::MetadataError;

/// Describes where a package was sourced from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PackageSource {
    /// Published on crates.io (or another registry).
    CratesIo,
    /// Fetched from a git repository.
    Git { url: String },
    /// A local path dependency.
    Path { path: String },
    /// Source could not be determined.
    Unknown,
}

/// Metadata about a single third-party package in the dependency tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    /// Crate name (e.g. "serde").
    pub name: String,
    /// Semver version string (e.g. "1.0.210").
    pub version: String,
    /// Where the package was obtained from.
    pub source: PackageSource,
    /// SPDX license expression, if declared in the manifest.
    pub license: Option<String>,
    /// Absolute path to the package's `Cargo.toml`.
    pub manifest_path: String,
}

/// Resolve the source kind from the raw `Source::repr` string that
/// `cargo metadata` provides.
fn classify_source(repr: &str) -> PackageSource {
    if repr.contains("registry") {
        PackageSource::CratesIo
    } else if repr.starts_with("git+") {
        let url = repr.strip_prefix("git+").unwrap_or(repr).to_string();
        PackageSource::Git { url }
    } else if repr.starts_with("path+") {
        let path = repr.strip_prefix("path+").unwrap_or(repr).to_string();
        PackageSource::Path { path }
    } else {
        PackageSource::Unknown
    }
}

/// Collect metadata for every **third-party** (non-workspace) package in the
/// resolved dependency graph.
///
/// If `manifest_path` is `Some`, the given `Cargo.toml` is used as the entry
/// point; otherwise `MetadataCommand` will look for a manifest in the current
/// working directory.
pub fn get_all_packages(manifest_path: Option<&str>) -> Result<Vec<PackageInfo>, MetadataError> {
    let mut cmd = MetadataCommand::new();
    if let Some(path) = manifest_path {
        cmd.manifest_path(path);
    }

    let metadata = cmd.exec()?;

    // Build a set of workspace member IDs so we can skip them.
    let workspace_ids: std::collections::HashSet<_> = metadata.workspace_members.iter().collect();

    let packages: Vec<PackageInfo> = metadata
        .packages
        .iter()
        .filter(|pkg| !workspace_ids.contains(&pkg.id))
        .map(|pkg| {
            let source = match &pkg.source {
                Some(src) => classify_source(&src.repr),
                // Workspace members typically have source = None, but we
                // already filtered those out above. A remaining None source
                // is treated as crates.io (rare edge-case).
                None => PackageSource::CratesIo,
            };

            PackageInfo {
                name: pkg.name.clone(),
                version: pkg.version.to_string(),
                source,
                license: pkg.license.clone(),
                manifest_path: pkg.manifest_path.to_string(),
            }
        })
        .collect();

    Ok(packages)
}

/// Identify crates that appear more than once (with different versions) in the
/// dependency tree.
///
/// Returns a vec of `(crate_name, versions)` sorted by crate name. Each
/// `versions` list is sorted as well.
pub fn find_duplicate_versions(packages: &[PackageInfo]) -> Vec<(String, Vec<String>)> {
    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();

    for pkg in packages {
        by_name
            .entry(pkg.name.as_str())
            .or_default()
            .push(pkg.version.as_str());
    }

    let mut duplicates: Vec<(String, Vec<String>)> = by_name
        .into_iter()
        .filter(|(_, versions)| versions.len() > 1)
        .map(|(name, mut versions)| {
            versions.sort();
            versions.dedup();
            (
                name.to_string(),
                versions.into_iter().map(String::from).collect::<Vec<_>>(),
            )
        })
        // Only keep entries that truly have more than one *distinct* version.
        .filter(|(_, versions)| versions.len() > 1)
        .collect();

    duplicates.sort_by(|a, b| a.0.cmp(&b.0));
    duplicates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_registry_source() {
        let src = "registry+https://github.com/rust-lang/crates.io-index";
        assert_eq!(classify_source(src), PackageSource::CratesIo);
    }

    #[test]
    fn classify_git_source() {
        let src = "git+https://github.com/foo/bar.git#abc123";
        match classify_source(src) {
            PackageSource::Git { url } => {
                assert!(url.contains("github.com/foo/bar"));
            }
            other => panic!("expected Git, got {:?}", other),
        }
    }

    #[test]
    fn classify_path_source() {
        let src = "path+file:///home/user/my-crate";
        match classify_source(src) {
            PackageSource::Path { path } => {
                assert!(path.contains("my-crate"));
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn classify_unknown_source() {
        assert_eq!(classify_source("something-else"), PackageSource::Unknown);
    }

    #[test]
    fn find_duplicates_basic() {
        let packages = vec![
            PackageInfo {
                name: "serde".into(),
                version: "1.0.100".into(),
                source: PackageSource::CratesIo,
                license: Some("MIT OR Apache-2.0".into()),
                manifest_path: "/fake/serde-1/Cargo.toml".into(),
            },
            PackageInfo {
                name: "serde".into(),
                version: "1.0.210".into(),
                source: PackageSource::CratesIo,
                license: Some("MIT OR Apache-2.0".into()),
                manifest_path: "/fake/serde-2/Cargo.toml".into(),
            },
            PackageInfo {
                name: "tokio".into(),
                version: "1.38.0".into(),
                source: PackageSource::CratesIo,
                license: Some("MIT".into()),
                manifest_path: "/fake/tokio/Cargo.toml".into(),
            },
        ];

        let dups = find_duplicate_versions(&packages);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].0, "serde");
        assert_eq!(dups[0].1, vec!["1.0.100", "1.0.210"]);
    }

    #[test]
    fn find_duplicates_empty() {
        let packages: Vec<PackageInfo> = vec![];
        assert!(find_duplicate_versions(&packages).is_empty());
    }

    #[test]
    fn find_duplicates_no_dups() {
        let packages = vec![
            PackageInfo {
                name: "serde".into(),
                version: "1.0.210".into(),
                source: PackageSource::CratesIo,
                license: Some("MIT OR Apache-2.0".into()),
                manifest_path: "/fake/serde/Cargo.toml".into(),
            },
            PackageInfo {
                name: "tokio".into(),
                version: "1.38.0".into(),
                source: PackageSource::CratesIo,
                license: Some("MIT".into()),
                manifest_path: "/fake/tokio/Cargo.toml".into(),
            },
        ];
        assert!(find_duplicate_versions(&packages).is_empty());
    }
}
