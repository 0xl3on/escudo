use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::time::Duration;

use crate::config::EscudoConfig;
use crate::error::FreshnessError;
use crate::metadata::{PackageInfo, PackageSource};

fn is_valid_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A crate version that was published too recently (within its cooldown window).
#[derive(Debug, Clone)]
pub struct FreshnessViolation {
    pub crate_name: String,
    pub version: String,
    pub published: DateTime<Utc>,
    pub age_days: i64,
    pub cooldown_days: u32,
}

/// A crate version that could not be verified against the crates.io API.
#[derive(Debug, Clone)]
pub struct UnverifiedCrate {
    pub crate_name: String,
    pub version: String,
}

/// Combined result of freshness checks.
pub struct FreshnessResult {
    pub violations: Vec<FreshnessViolation>,
    pub unverified: Vec<UnverifiedCrate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionInfo {
    num: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CrateVersionsCache {
    fetched_at: DateTime<Utc>,
    versions: Vec<VersionInfo>,
}

/// Max concurrent requests to crates.io (be respectful).
const MAX_CONCURRENT_REQUESTS: usize = 10;

/// Per-request timeout. Fail fast — a hanging API is as bad as a down one.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Connection timeout for the HTTP client.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Max retries on 429 rate limit responses.
const MAX_RETRIES: u32 = 5;

/// Initial backoff on 429 (doubles each retry).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize)]
struct CratesIoVersionsResponse {
    versions: Vec<CratesIoVersion>,
}

#[derive(Debug, Deserialize)]
struct CratesIoVersion {
    num: String,
    created_at: DateTime<Utc>,
}

/// Check every `CratesIo`-sourced package against its freshness cooldown.
///
/// Fetches version info concurrently (up to [`MAX_CONCURRENT_REQUESTS`] in
/// flight) and uses a disk cache to avoid redundant API calls.
pub async fn check_freshness(
    config: &EscudoConfig,
    packages: &[PackageInfo],
) -> Result<FreshnessResult, FreshnessError> {
    let client = reqwest::Client::builder()
        .user_agent(format!("escudo/{}", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(FreshnessError::HttpClient)?;

    let cache_dir = cache_directory()?;
    std::fs::create_dir_all(&cache_dir)?;

    let now = Utc::now();

    // Collect (crate_name -> set of versions needed) for all crates.io deps.
    let mut needed: HashMap<String, Vec<String>> = HashMap::new();
    for p in packages
        .iter()
        .filter(|p| p.source == PackageSource::CratesIo)
    {
        if !is_valid_crate_name(&p.name) {
            return Err(FreshnessError::InvalidCrateName(p.name.clone()));
        }
        needed
            .entry(p.name.clone())
            .or_default()
            .push(p.version.clone());
    }

    // Check which crates need a fresh fetch (cache miss or needed version not in cache).
    let mut to_fetch_names: Vec<String> = Vec::new();
    for (name, versions) in &needed {
        let cache_path = cache_dir.join(format!("{}.json", name));
        let has_all = load_cache(&cache_path)
            .ok()
            .flatten()
            .is_some_and(|cached| {
                versions
                    .iter()
                    .all(|v| cached.versions.iter().any(|cv| cv.num == *v))
            });
        if !has_all {
            to_fetch_names.push(name.clone());
        }
    }

    let total = needed.len();
    let to_fetch = to_fetch_names.len();
    let cached = total - to_fetch;

    if to_fetch > 0 {
        eprintln!(
            "checking {} crates against crates.io ({} cached, {} to fetch)...",
            total, cached, to_fetch
        );
    } else {
        eprintln!("checking {} crates (all cached)", total);
    }

    // Load cached crates into the version map first.
    let mut version_map: HashMap<String, Vec<VersionInfo>> = HashMap::new();
    for name in needed.keys() {
        let cache_path = cache_dir.join(format!("{}.json", name));
        if let Some(cached) = load_cache(&cache_path)? {
            if !to_fetch_names.contains(name) {
                version_map.insert(name.clone(), cached.versions);
            }
        }
    }

    // Fetch remaining crates concurrently.
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let client = Arc::new(client);
    let cache_dir = Arc::new(cache_dir);

    let mut handles = Vec::with_capacity(to_fetch_names.len());
    for name in to_fetch_names {
        let sem = Arc::clone(&semaphore);
        let cli = Arc::clone(&client);
        let cdir = Arc::clone(&cache_dir);
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let versions = fetch_versions(&cli, &cdir, &name).await;
            (name, versions)
        }));
    }

    // Collect results into a lookup map: crate_name -> Vec<VersionInfo>.
    // Errors are fatal — if we can't verify a crate, the audit must fail.
    let mut version_map: HashMap<String, Vec<VersionInfo>> = HashMap::new();
    for handle in handles {
        let (name, result) = handle.await.map_err(|_| FreshnessError::TaskPanicked)?;
        let versions = result?;
        version_map.insert(name, versions);
    }

    let mut violations = Vec::new();
    let mut unverified = Vec::new();

    for pkg in packages {
        if pkg.source != PackageSource::CratesIo {
            continue;
        }

        let cooldown_days = config
            .exceptions
            .iter()
            .find(|e| e.crate_name == pkg.name)
            .map(|e| e.days)
            .unwrap_or(config.cooldown_days);

        let Some(versions) = version_map.get(&pkg.name) else {
            // API was queried but this crate wasn't in the result — should
            // not happen since we deduplicated, but treat as unverified.
            unverified.push(UnverifiedCrate {
                crate_name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
            continue;
        };

        let Some(vi) = versions.iter().find(|v| v.num == pkg.version) else {
            // The crate exists on crates.io but this specific version is
            // missing from the API response. Could be yanked, or a
            // compromised API omitting versions to bypass checks.
            unverified.push(UnverifiedCrate {
                crate_name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
            continue;
        };

        // Reject future publish dates — a compromised API could set
        // created_at far in the future to make a crate appear old.
        if vi.created_at > now {
            unverified.push(UnverifiedCrate {
                crate_name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
            continue;
        }

        let age_days = now.signed_duration_since(vi.created_at).num_days();
        if age_days < cooldown_days as i64 {
            violations.push(FreshnessViolation {
                crate_name: pkg.name.clone(),
                version: pkg.version.clone(),
                published: vi.created_at,
                age_days,
                cooldown_days,
            });
        }
    }

    Ok(FreshnessResult {
        violations,
        unverified,
    })
}

/// Fetch version list from crates.io and persist to disk cache.
async fn fetch_versions(
    client: &reqwest::Client,
    cache_dir: &std::path::Path,
    crate_name: &str,
) -> Result<Vec<VersionInfo>, FreshnessError> {
    let cache_path = cache_dir.join(format!("{}.json", crate_name));

    let url = format!("https://crates.io/api/v1/crates/{}/versions", crate_name);

    // Retry with exponential backoff on 429 rate limits.
    let mut backoff = INITIAL_BACKOFF;
    for attempt in 0..=MAX_RETRIES {
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| FreshnessError::HttpRequest {
                crate_name: crate_name.to_string(),
                source: e,
            })?;

        let status = resp.status();

        let is_retryable =
            status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();

        if !status.is_success() {
            if is_retryable && attempt < MAX_RETRIES {
                eprintln!(
                    "HTTP {} on `{}`, retrying in {}s (attempt {}/{})",
                    status.as_u16(),
                    crate_name,
                    backoff.as_secs(),
                    attempt + 1,
                    MAX_RETRIES
                );
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            return Err(FreshnessError::HttpStatus {
                crate_name: crate_name.to_string(),
                status,
            });
        }

        // Success — parse, validate, and cache.
        let body: CratesIoVersionsResponse =
            resp.json()
                .await
                .map_err(|e| FreshnessError::ParseResponse {
                    crate_name: crate_name.to_string(),
                    source: e,
                })?;

        let versions: Vec<VersionInfo> = body
            .versions
            .into_iter()
            .map(|v| VersionInfo {
                num: v.num,
                created_at: v.created_at,
            })
            .collect();

        // Don't cache empty responses — could be an API anomaly.
        if !versions.is_empty() {
            let entry = CrateVersionsCache {
                fetched_at: Utc::now(),
                versions: versions.clone(),
            };
            let _ = save_cache(&cache_path, &entry);
        }

        return Ok(versions);
    }

    // Should never reach here, but if it does, treat as rate limit failure.
    Err(FreshnessError::HttpStatus {
        crate_name: crate_name.to_string(),
        status: reqwest::StatusCode::TOO_MANY_REQUESTS,
    })
}

/// Return the cache directory path.
///
/// Precedence:
/// 1. `ESCUDO_CACHE_DIR` environment variable (for CI persistence)
/// 2. `<platform cache dir>/escudo` (e.g. `~/.cache/escudo` on Linux)
fn cache_directory() -> Result<PathBuf, FreshnessError> {
    if let Ok(dir) = std::env::var("ESCUDO_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::cache_dir().ok_or(FreshnessError::NoCacheDir)?;
    Ok(base.join("escudo"))
}

/// Try to load and deserialize a cache file. Cache entries never expire —
/// publish dates for a given version are immutable on crates.io.
fn load_cache(path: &std::path::Path) -> Result<Option<CrateVersionsCache>, FreshnessError> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(path)?;

    let entry: CrateVersionsCache =
        serde_json::from_str(&contents).map_err(|e| FreshnessError::CacheParse {
            path: path.display().to_string(),
            source: e,
        })?;

    Ok(Some(entry))
}

/// Persist a [`CrateVersionsCache`] to disk as JSON.
fn save_cache(path: &std::path::Path, entry: &CrateVersionsCache) -> Result<(), FreshnessError> {
    let json = serde_json::to_string_pretty(entry).map_err(|e| FreshnessError::CacheParse {
        path: path.display().to_string(),
        source: e,
    })?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_directory_defaults_to_escudo() {
        // Only test the default path (no env mutation needed).
        // ESCUDO_CACHE_DIR is not expected to be set during tests.
        if std::env::var("ESCUDO_CACHE_DIR").is_err() {
            let dir = cache_directory().unwrap();
            assert!(dir.ends_with("escudo"));
        }
    }

    #[test]
    fn test_load_cache_returns_none_for_missing_file() {
        let path = PathBuf::from("/tmp/escudo_test_nonexistent_cache_file.json");
        let result = load_cache(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_cache_round_trip() {
        let dir = std::env::temp_dir().join("escudo_freshness_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_crate.json");

        let now = Utc::now();
        let entry = CrateVersionsCache {
            fetched_at: now,
            versions: vec![VersionInfo {
                num: "1.0.0".to_string(),
                created_at: now,
            }],
        };

        save_cache(&path, &entry).unwrap();

        let loaded = load_cache(&path).unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.versions.len(), 1);
        assert_eq!(loaded.versions[0].num, "1.0.0");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_freshness_violation_fields() {
        let now = Utc::now();
        let v = FreshnessViolation {
            crate_name: "foo".to_string(),
            version: "0.1.0".to_string(),
            published: now,
            age_days: 2,
            cooldown_days: 7,
        };
        assert_eq!(v.crate_name, "foo");
        assert_eq!(v.age_days, 2);
        assert_eq!(v.cooldown_days, 7);
    }
}
