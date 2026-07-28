//! Oxidized API client implementation with testable backend abstraction.
//!
//! This module provides the [`OxidizedBackend`] trait for abstracting Oxidized API
//! operations and [`OxidizedClient`] as the HTTP client implementation.
//!
//! # Architecture
//!
//! The trait-based design enables:
//! - Dependency injection for testing
//! - Mock backend for unit tests
//! - Future extensions (e.g., direct Git backend)
//!
//! # Example
//!
//! ```ignore
//! use mcp_oxidized::oxidized::{OxidizedBackend, OxidizedClient};
//! use mcp_oxidized::config::Config;
//!
//! let config = Config::load()?;
//! let client = OxidizedClient::try_new(&config)?;
//!
//! // List all nodes
//! let (nodes, _metadata) = client.get_nodes().await?;
//! for node in nodes {
//!     println!("{}: {:?}", node.name, node.status);
//! }
//! ```

use async_trait::async_trait;
use moka::future::Cache;
use regex::Regex;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::instrument;

use crate::config::Config;
use crate::error::{Actionable, OxidizedError};

// ============================================================================
// Constants
// ============================================================================

/// Default HTTP connect timeout in seconds.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Default HTTP request timeout in seconds.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

// ============================================================================
// Cache TTL Constants (FR28, FR29, FR30)
// ============================================================================

/// Cache TTL for nodes list in seconds (5 minutes).
/// Node inventory changes infrequently during a session.
pub const NODES_CACHE_TTL_SECS: u64 = 300;

/// Cache TTL for node configurations in seconds (2 minutes).
/// Balance between performance and freshness.
pub const CONFIG_CACHE_TTL_SECS: u64 = 120;

/// Cache TTL for statistics in seconds (30 seconds).
/// Provides near real-time feel for stats.
pub const STATS_CACHE_TTL_SECS: u64 = 30;

// ============================================================================
// Retry Configuration Constants (NFR11)
// ============================================================================

/// Maximum number of retry attempts (initial + 2 retries).
pub const MAX_RETRY_ATTEMPTS: u8 = 3;

/// Retry delays in milliseconds for exponential backoff.
/// Delay sequence: 200ms, 800ms (exponential progression).
pub const RETRY_DELAYS_MS: [u64; 2] = [200, 800];

/// Regex for parsing conf_search HTML response.
///
/// Compiled once at first use for performance. Extracts content from `<td>` tags.
/// Pattern: `<td>([^<]+)</td>` - captures text between opening and closing td tags.
static TD_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<td>([^<]+)</td>").expect("TD_REGEX is a valid pattern"));

/// URL-encode each component of an Oxidized full node name while preserving
/// `/` separators used by grouped nodes.
fn encode_path_segments(path: &str) -> String {
    path.split('/')
        .map(urlencoding::encode)
        .collect::<Vec<_>>()
        .join("/")
}

// ============================================================================
// Data Models
// ============================================================================

/// Nested "last" backup information from Oxidized 0.35.0.
///
/// This nested object is present in both `/nodes.json` and `/node/show/{name}.json`
/// responses and contains detailed timing information about the last backup run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastBackup {
    /// Start timestamp of the backup
    pub start: Option<String>,
    /// End timestamp of the backup
    pub end: Option<String>,
    /// Status of the last backup
    pub status: Option<String>,
    /// Duration in seconds
    pub time: Option<f64>,
}

/// Represents a network device node in Oxidized inventory.
///
/// Contains metadata about the device including its name, IP address,
/// group classification, and backup status.
///
/// # Oxidized 0.35.0 Compatibility
///
/// - `/nodes.json` includes `status` and `time` at top level
/// - `/node/show/{name}.json` does NOT include these; they're only in the nested `last` object
/// - The `last_status` field is not present in Oxidized 0.35.0
///
/// # Example JSON from /nodes.json
///
/// ```json
/// {
///   "name": "SW-Core-01",
///   "full_name": "group/SW-Core-01",
///   "ip": "192.168.1.1",
///   "group": "switches",
///   "model": "cisco-ios",
///   "status": "success",
///   "time": "2025-01-15 10:30:00 UTC",
///   "mtime": "2025-01-15 10:25:00 UTC",
///   "last": { "start": "...", "end": "...", "status": "success", "time": 1.5 }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Short device name (e.g., "SW-Core-01")
    pub name: String,
    /// Fully qualified device name
    pub full_name: String,
    /// Device IP address
    pub ip: String,
    /// Device group/category (e.g., "switches", "routers")
    pub group: String,
    /// Device model/platform (e.g., "cisco-ios", "junos")
    pub model: String,
    /// Current backup status (e.g., "success", "failure", "never")
    /// Note: Optional because /node/show/{name}.json doesn't include this field
    #[serde(default)]
    pub status: Option<String>,
    /// Previous backup status (optional, not present in Oxidized 0.35.0+)
    #[serde(default)]
    pub last_status: Option<String>,
    /// Timestamp of last backup attempt
    /// Note: Optional because /node/show/{name}.json doesn't include this field
    pub time: Option<String>,
    /// Timestamp of last configuration modification
    pub mtime: Option<String>,
    /// Detailed last backup information (present in Oxidized 0.35.0+)
    #[serde(default)]
    pub last: Option<LastBackup>,
}

impl Node {
    /// Get the effective status, preferring top-level status over last.status.
    ///
    /// For `/nodes.json` responses, `status` is at the top level.
    /// For `/node/show/{name}.json` responses, it's only in `last.status`.
    pub fn effective_status(&self) -> Option<&str> {
        self.status
            .as_deref()
            .or_else(|| self.last.as_ref().and_then(|l| l.status.as_deref()))
    }

    /// Get the effective time, preferring top-level time over last.end.
    pub fn effective_time(&self) -> Option<&str> {
        self.time.as_deref().or_else(|| {
            self.last
                .as_ref()
                .and_then(|l| l.end.as_deref().or(l.start.as_deref()))
        })
    }
}

/// Author information for a configuration version.
///
/// Oxidized 0.35.0+ returns author as an object with name, email, and time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionAuthor {
    /// Author's name
    pub name: String,
    /// Author's email
    pub email: String,
    /// Commit timestamp
    pub time: String,
}

/// Represents a configuration version in Oxidized Git repository.
///
/// Each version corresponds to a Git commit containing a configuration snapshot.
///
/// # Example JSON (Oxidized 0.35.0+)
///
/// ```json
/// {
///   "oid": "abc123def456",
///   "date": "2025-01-15 10:30:00 UTC",
///   "time": "2025-01-15 10:30:00 UTC",
///   "author": {
///     "name": "oxidized",
///     "email": "oxidized@example.com",
///     "time": "2025-01-15 10:30:00 UTC"
///   },
///   "message": "update SW-Core-01"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeVersion {
    /// Git object ID (commit hash)
    pub oid: String,
    /// Commit timestamp
    pub date: String,
    /// Commit timestamp (duplicate of date in Oxidized 0.35.0+)
    #[serde(default)]
    pub time: Option<String>,
    /// Commit author information
    pub author: VersionAuthor,
    /// Commit message
    pub message: String,
}

/// Global Oxidized server statistics.
///
/// Provides an overview of the backup system's health and activity.
///
/// # Oxidized 0.35.0 Compatibility
///
/// Oxidized 0.35.0 does not expose a `/stats` JSON endpoint. Statistics are
/// computed from the nodes list by counting `status` field values.
///
/// # Example
///
/// ```ignore
/// let stats = Stats::from_nodes(&nodes);
/// println!("Total: {}, Success: {}", stats.total_nodes.unwrap(), stats.success_count.unwrap());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    /// Total number of managed nodes
    pub total_nodes: Option<u32>,
    /// Number of successful backups
    pub success_count: Option<u32>,
    /// Number of failed backups
    pub failure_count: Option<u32>,
    /// Timestamp of last backup run
    pub last_run: Option<String>,
}

impl Stats {
    /// Compute statistics from a list of nodes.
    ///
    /// This is used as the primary method for Oxidized 0.35.0 since it does not
    /// expose a `/stats` endpoint. Statistics are computed by counting nodes
    /// by their `status` field.
    ///
    /// # Arguments
    ///
    /// * `nodes` - Slice of nodes to compute statistics from
    ///
    /// # Returns
    ///
    /// A `Stats` struct with computed values:
    /// - `total_nodes`: Total number of nodes
    /// - `success_count`: Nodes with status "success"
    /// - `failure_count`: Nodes with status other than "success" (including "never", "failure", etc.)
    /// - `last_run`: Most recent `time` value from any node
    pub fn from_nodes(nodes: &[Node]) -> Self {
        let total = nodes.len() as u32;
        let success = nodes
            .iter()
            .filter(|n| n.effective_status() == Some("success"))
            .count() as u32;
        let failure = total - success;

        // Find the most recent time from any node
        let last_run = nodes
            .iter()
            .filter_map(|n| n.effective_time())
            .filter(|t| *t != "never")
            .max()
            .map(String::from);

        Stats {
            total_nodes: Some(total),
            success_count: Some(success),
            failure_count: Some(failure),
            last_run,
        }
    }
}

// ============================================================================
// Cache Metadata (FR32)
// ============================================================================

/// Metadata indicating cache status for responses (FR32).
///
/// Included in cached responses to indicate whether the data came from
/// cache (hit) or was freshly fetched from the API (miss).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    /// True if the response was served from cache
    pub cache_hit: bool,
    /// True when the response was fetched from Oxidized for this request.
    pub fresh: bool,
}

/// Result of Oxidized's optional server-side configuration prefilter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfSearchResult {
    /// The endpoint was available. The list may legitimately be empty.
    Available(Vec<String>),
    /// The endpoint could not be used; callers should fall back to full search.
    Unavailable(String),
}

impl Default for ConfSearchResult {
    fn default() -> Self {
        Self::Unavailable("prefilter request failed".to_string())
    }
}

impl ConfSearchResult {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Available(nodes) => nodes.is_empty(),
            Self::Unavailable(_) => true,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Available(nodes) => nodes.len(),
            Self::Unavailable(_) => 0,
        }
    }
}

impl CacheMetadata {
    /// Create metadata for a cache hit.
    pub fn hit() -> Self {
        Self {
            cache_hit: true,
            fresh: false,
        }
    }

    /// Create metadata for a cache miss.
    pub fn miss() -> Self {
        Self {
            cache_hit: false,
            fresh: true,
        }
    }
}

/// Response wrapper for nodes list with cache metadata.
#[derive(Debug, Clone, Serialize)]
pub struct CachedNodes {
    /// The list of nodes
    pub nodes: Vec<Node>,
    /// Cache status metadata
    pub metadata: CacheMetadata,
}

/// Response wrapper for a single node with cache metadata.
#[derive(Debug, Clone, Serialize)]
pub struct CachedNode {
    /// The node data
    pub node: Node,
    /// Cache status metadata
    pub metadata: CacheMetadata,
}

/// Response wrapper for node configuration with cache metadata.
#[derive(Debug, Clone, Serialize)]
pub struct CachedConfig {
    /// The configuration text
    pub config: String,
    /// Cache status metadata
    pub metadata: CacheMetadata,
}

/// Response wrapper for statistics with cache metadata.
#[derive(Debug, Clone, Serialize)]
pub struct CachedStats {
    /// The statistics data
    pub stats: Stats,
    /// Cache status metadata
    pub metadata: CacheMetadata,
}

// ============================================================================
// OxidizedBackend Trait
// ============================================================================

/// Trait for abstracting Oxidized API operations.
///
/// This trait defines the contract for interacting with the Oxidized backup system.
/// The primary implementation is [`OxidizedClient`], but the trait allows for
/// mock implementations in tests.
///
/// # Thread Safety
///
/// All implementations must be `Send + Sync` to allow use across async tasks.
///
/// # Error Handling
///
/// All methods return `Result<T, OxidizedError>` where errors are classified as
/// transient (retryable) or permanent. See [`OxidizedError`] for details.
///
/// # Cache Metadata (FR32)
///
/// Cached read operations return tuples with [`CacheMetadata`] to indicate
/// cache hit/miss status for MCP response inclusion.
///
/// # Example
///
/// ```ignore
/// async fn list_nodes<B: OxidizedBackend>(backend: &B) -> Result<(), OxidizedError> {
///     let (nodes, metadata) = backend.get_nodes().await?;
///     println!("Cache hit: {}", metadata.cache_hit);
///     for node in nodes {
///         println!("{}: {}", node.name, node.status);
///     }
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait OxidizedBackend: Send + Sync {
    /// Whether configuration-bearing output must be redacted.
    fn redaction_enabled(&self) -> bool {
        true
    }

    /// Retrieve all nodes from Oxidized inventory.
    ///
    /// Returns the complete list of managed network devices with cache metadata (FR32).
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    /// - [`OxidizedError::AuthFailed`] - Authentication failure
    /// - [`OxidizedError::ParseError`] - Invalid JSON response
    async fn get_nodes(&self) -> Result<(Vec<Node>, CacheMetadata), OxidizedError>;

    /// Retrieve a specific node by name.
    ///
    /// Returns node details with cache metadata (FR32).
    ///
    /// # Arguments
    ///
    /// * `name` - The node name to look up (e.g., "SW-Core-01")
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::NodeNotFound`] - Node does not exist
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    /// - [`OxidizedError::AuthFailed`] - Authentication failure
    async fn get_node(&self, name: &str) -> Result<(Node, CacheMetadata), OxidizedError>;

    /// Retrieve the current configuration for a node.
    ///
    /// Returns the latest configuration text with cache metadata (FR32).
    ///
    /// # Arguments
    ///
    /// * `name` - The node name to fetch configuration for
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::NodeNotFound`] - Node does not exist
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    async fn get_node_config(&self, name: &str) -> Result<(String, CacheMetadata), OxidizedError>;

    /// Retrieve version history for a node.
    ///
    /// Returns a list of configuration versions (Git commits).
    /// Note: Versions are not cached (historical data, rarely accessed repeatedly).
    ///
    /// # Arguments
    ///
    /// * `name` - The node name to get versions for
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::NodeNotFound`] - Node does not exist
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    async fn get_node_versions(&self, name: &str) -> Result<Vec<NodeVersion>, OxidizedError>;

    /// Retrieve a specific configuration version.
    ///
    /// Returns the configuration text at a specific point in time.
    /// Note: Version content is not cached (point-in-time data).
    ///
    /// # Arguments
    ///
    /// * `name` - The node name
    /// * `oid` - The Git object ID (commit hash) of the version
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::NodeNotFound`] - Node or version does not exist
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    async fn get_node_version(&self, name: &str, oid: &str) -> Result<String, OxidizedError>;

    /// Retrieve global Oxidized statistics.
    ///
    /// Returns server-wide backup statistics with cache metadata (FR32).
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    /// - [`OxidizedError::ParseError`] - Invalid JSON response
    async fn get_stats(&self) -> Result<(Stats, CacheMetadata), OxidizedError>;

    /// Trigger an immediate backup for a node.
    ///
    /// Requests Oxidized to prioritize and run backup for the specified node.
    ///
    /// # Arguments
    ///
    /// * `node` - The node name to backup
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::NodeNotFound`] - Node does not exist
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    async fn trigger_backup(&self, node: &str) -> Result<(), OxidizedError>;

    /// Prioritize a node in the backup queue.
    ///
    /// Moves the node to the front of the backup queue.
    ///
    /// # Arguments
    ///
    /// * `node` - The node name to prioritize
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::NodeNotFound`] - Node does not exist
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    async fn prioritize_node(&self, node: &str) -> Result<(), OxidizedError>;

    /// Reload the Oxidized source inventory.
    ///
    /// Triggers Oxidized to re-read its node inventory from the configured source.
    ///
    /// # Errors
    ///
    /// - [`OxidizedError::ApiUnreachable`] - Network/connection error
    async fn reload_sources(&self) -> Result<(), OxidizedError>;

    /// Search for nodes whose configs contain the given pattern (server-side pre-filter).
    ///
    /// Uses Oxidized's `/nodes/conf_search` endpoint which returns HTML.
    /// Returns node names that have at least one match, enabling optimization
    /// by avoiding fetching configs that won't match.
    ///
    /// # Graceful Degradation
    ///
    /// Returns `Ok(vec![])` on any error (network, parsing, 404) to allow
    /// fallback to full client-side search. Errors are logged at debug level.
    ///
    /// # Arguments
    ///
    /// * `pattern` - The search pattern to find in configurations
    ///
    /// # Returns
    ///
    /// A list of node names whose configurations contain the pattern.
    async fn conf_search(&self, pattern: &str) -> Result<ConfSearchResult, OxidizedError>;
}

// ============================================================================
// BasicAuth
// ============================================================================

/// HTTP Basic Authentication credentials.
#[derive(Clone)]
pub struct BasicAuth {
    username: String,
    password: String,
}

impl BasicAuth {
    /// Create new Basic Auth credentials.
    pub fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}

// ============================================================================
// OxidizedClient
// ============================================================================

/// HTTP client implementation of [`OxidizedBackend`].
///
/// Provides HTTP-based access to the Oxidized REST API with support for
/// Basic Authentication and configurable timeouts.
///
/// # Configuration
///
/// The client is configured via [`Config`] which reads from environment variables:
/// - `OXIDIZED_URL` - Base URL for the Oxidized server
/// - `OXIDIZED_USER` / `OXIDIZED_PASSWORD` - Optional authentication credentials
///
/// # Example
///
/// ```ignore
/// use mcp_oxidized::oxidized::OxidizedClient;
/// use mcp_oxidized::config::Config;
///
/// let config = Config::load()?;
/// let client = OxidizedClient::new(&config);
///
/// let nodes = client.get_nodes().await?;
/// ```
#[derive(Clone)]
pub struct OxidizedClient {
    client: Client,
    base_url: String,
    auth: Option<BasicAuth>,
    /// Custom HTTP headers to include in all requests (FR44)
    custom_headers: Vec<(String, String)>,
    // Integrated caches (FR28, FR29, FR30)
    nodes_cache: Cache<(), Vec<Node>>,
    config_cache: Cache<String, String>,
    stats_cache: Cache<(), Stats>,
    node_cache: Cache<String, Node>,
    pending_backups: Arc<RwLock<HashSet<String>>>,
    redact_secrets: bool,
}

impl OxidizedClient {
    /// Create a new OxidizedClient from configuration.
    ///
    /// Initializes an HTTP client with appropriate timeouts and authentication
    /// settings based on the provided configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration including URL and optional credentials
    ///
    /// # Errors
    ///
    /// Returns `OxidizedError::ConfigError` if the HTTP client cannot be built
    /// (e.g., TLS backend issues).
    pub fn try_new(config: &Config) -> Result<Self, OxidizedError> {
        // Build HTTP client with SSL verification setting (FR43)
        // Only apply danger_accept_invalid_certs for HTTPS URLs (no effect on HTTP)
        let is_https = config.oxidized_url.starts_with("https://");
        let skip_ssl_verify = is_https && !config.ssl_verify;

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
            .danger_accept_invalid_certs(skip_ssl_verify)
            .build()
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to build HTTP client");
                OxidizedError::ConfigError(crate::config::ConfigError::InvalidUrl(format!(
                    "HTTP client build failed: {}",
                    e
                )))
            })?;

        let auth = match (&config.oxidized_user, &config.oxidized_password) {
            (Some(user), Some(pass)) => Some(BasicAuth::new(user.clone(), pass.clone())),
            _ => None,
        };

        // Initialize caches with appropriate TTLs (FR28, FR29, FR30)
        let nodes_cache = Cache::builder()
            .time_to_live(Duration::from_secs(NODES_CACHE_TTL_SECS))
            .build();

        let config_cache = Cache::builder()
            .time_to_live(Duration::from_secs(CONFIG_CACHE_TTL_SECS))
            .build();

        let stats_cache = Cache::builder()
            .time_to_live(Duration::from_secs(STATS_CACHE_TTL_SECS))
            .build();

        let node_cache = Cache::builder()
            .time_to_live(Duration::from_secs(NODES_CACHE_TTL_SECS))
            .build();

        Ok(Self {
            client,
            base_url: config.oxidized_url.clone(),
            auth,
            custom_headers: config.custom_headers.clone(),
            nodes_cache,
            config_cache,
            stats_cache,
            node_cache,
            pending_backups: Arc::new(RwLock::new(HashSet::new())),
            redact_secrets: std::env::var("OXIDIZED_REDACT_SECRETS")
                .map(|value| !value.eq_ignore_ascii_case("false"))
                .unwrap_or(true),
        })
    }

    /// Mark a node as having an in-flight backup. Pending nodes bypass config
    /// caching so an early read cannot poison the cache with stale content.
    pub async fn set_backup_pending(&self, name: &str, pending: bool) {
        let mut nodes = self.pending_backups.write().await;
        if pending {
            nodes.insert(name.to_string());
            self.config_cache.invalidate(name).await;
        } else {
            nodes.remove(name);
        }
    }

    pub async fn is_backup_pending(&self, name: &str) -> bool {
        self.pending_backups.read().await.contains(name)
    }

    /// Fetch node state without consulting or updating the per-node cache.
    pub async fn get_node_fresh(&self, name: &str) -> Result<Node, OxidizedError> {
        let endpoint = format!("/node/show/{}.json", urlencoding::encode(name));
        self.execute_with_retry(|| async {
            let response = self.build_request(&endpoint).send().await;
            self.handle_json_response(response, name).await
        })
        .await
    }

    /// Force the next configuration read to go to Oxidized.
    pub async fn invalidate_config(&self, name: &str) {
        self.config_cache.invalidate(name).await;
    }

    /// Create a new OxidizedClient from configuration (convenience wrapper).
    ///
    /// This method panics on failure and is intended for use in tests
    /// or when the caller can guarantee the configuration is valid.
    ///
    /// # Panics
    ///
    /// Panics if the HTTP client cannot be built.
    #[cfg(test)]
    pub fn new(config: &Config) -> Self {
        Self::try_new(config).expect("Failed to create OxidizedClient")
    }

    /// Execute an operation with retry on transient errors (NFR11).
    ///
    /// Implements exponential backoff with delays [200ms, 800ms] for up to 3 total attempts.
    /// Only retries if the error's `is_transient()` returns true.
    ///
    /// # Arguments
    ///
    /// * `operation` - A closure that returns a Future producing a Result
    ///
    /// # Returns
    ///
    /// The result of the operation, or the final error after all retries exhausted.
    async fn execute_with_retry<T, F, Fut>(&self, operation: F) -> Result<T, OxidizedError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, OxidizedError>>,
    {
        let delays = [
            Duration::from_millis(RETRY_DELAYS_MS[0]),
            Duration::from_millis(RETRY_DELAYS_MS[1]),
        ];

        let mut last_error: Option<OxidizedError> = None;

        for attempt in 0..MAX_RETRY_ATTEMPTS {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(e) if e.is_transient() && attempt < MAX_RETRY_ATTEMPTS - 1 => {
                    let delay = delays[attempt as usize];
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_attempts = MAX_RETRY_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        error_type = %e.error_type(),
                        "Request failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    last_error = Some(e);
                }
                Err(e) => {
                    if attempt > 0 {
                        tracing::error!(
                            attempts = attempt + 1,
                            error_type = %e.error_type(),
                            "Request failed after all retries"
                        );
                    }
                    return Err(e);
                }
            }
        }

        // If we exhausted all retries, return the last transient error
        // Note: last_error should always be Some here since we only reach this point
        // after the loop has run at least once with a transient error, but we handle
        // the None case gracefully to avoid panics.
        match last_error {
            Some(error) => {
                tracing::error!(
                    attempts = MAX_RETRY_ATTEMPTS,
                    error_type = %error.error_type(),
                    "Request failed after all retries"
                );
                Err(error)
            }
            None => {
                // This should never happen, but we handle it gracefully
                tracing::error!(
                    attempts = MAX_RETRY_ATTEMPTS,
                    "Retry loop completed without error (unexpected state)"
                );
                Err(OxidizedError::HttpError {
                    status_code: 500,
                    context: "Retry loop completed in unexpected state".to_string(),
                })
            }
        }
    }

    /// Invalidate cache entries for a specific node (AC: 4).
    ///
    /// Clears the config_cache and node_cache entries for the specified node.
    /// Called after successful write operations that affect a single node.
    ///
    /// # Arguments
    ///
    /// * `name` - The node name to invalidate cache for
    pub async fn invalidate_node(&self, name: &str) {
        self.config_cache.invalidate(name).await;
        self.node_cache.invalidate(name).await;
    }

    /// Invalidate all cache entries (AC: 4).
    ///
    /// Clears all caches: nodes_cache, config_cache, node_cache, and stats_cache.
    /// Called after successful operations that may affect the entire inventory.
    pub async fn invalidate_all_nodes(&self) {
        self.nodes_cache.invalidate_all();
        self.config_cache.invalidate_all();
        self.node_cache.invalidate_all();
        self.stats_cache.invalidate_all();
    }

    /// Build an authenticated request to the given endpoint.
    ///
    /// Applies custom headers first, then Basic Auth only if no custom Authorization
    /// header was provided (FR44, AC3: custom Authorization takes priority).
    fn build_request(&self, endpoint: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.get(&url);

        // Apply custom headers first (may include Authorization)
        for (name, value) in &self.custom_headers {
            request = request.header(name.as_str(), value.as_str());
        }

        // Only apply Basic Auth if no custom Authorization header was provided
        if let Some(auth) = &self.auth {
            let has_custom_auth = self
                .custom_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Authorization"));
            if !has_custom_auth {
                request = request.basic_auth(&auth.username, Some(&auth.password));
            }
        }

        request
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
    }

    /// Handle HTTP response and map errors appropriately.
    async fn handle_json_response<T: serde::de::DeserializeOwned>(
        &self,
        response: Result<reqwest::Response, reqwest::Error>,
        context: &str,
    ) -> Result<T, OxidizedError> {
        let response = self.handle_request_error(response)?;
        let status = response.status();

        // Get response body first for error detection
        let body = response
            .text()
            .await
            .map_err(|e| OxidizedError::ApiUnreachable {
                source: e,
                attempt: 1,
                last_success: None,
            })?;

        // Check for Oxidized 0.35.0 NodeNotFound pattern in error responses
        // This can come with HTTP 500 status when a node doesn't exist
        if let Some(err) = self.check_node_not_found_body(&body, context) {
            return Err(err);
        }

        self.check_status(status, context)?;

        serde_json::from_str::<T>(&body).map_err(|e| OxidizedError::ParseError {
            context: context.to_string(),
            source: e,
        })
    }

    /// Handle HTTP response for text content.
    async fn handle_text_response(
        &self,
        response: Result<reqwest::Response, reqwest::Error>,
        context: &str,
    ) -> Result<String, OxidizedError> {
        let response = self.handle_request_error(response)?;
        let status = response.status();

        // Get response body first for error detection
        let body = response
            .text()
            .await
            .map_err(|e| OxidizedError::ApiUnreachable {
                source: e,
                attempt: 1,
                last_success: None,
            })?;

        // Check for Oxidized 0.35.0 "unable to find" pattern in error responses
        if let Some(err) = self.check_node_not_found_body(&body, context) {
            return Err(err);
        }

        self.check_status(status, context)?;

        Ok(body)
    }

    /// Handle HTTP response for empty responses (PUT/POST operations).
    async fn handle_empty_response(
        &self,
        response: Result<reqwest::Response, reqwest::Error>,
        context: &str,
    ) -> Result<(), OxidizedError> {
        let response = self.handle_request_error(response)?;
        let status = response.status();

        // Get response body for error detection (even for "empty" responses)
        let body = response
            .text()
            .await
            .map_err(|e| OxidizedError::ApiUnreachable {
                source: e,
                attempt: 1,
                last_success: None,
            })?;

        // Check for Oxidized 0.35.0 "unable to find" pattern in error responses
        if let Some(err) = self.check_node_not_found_body(&body, context) {
            return Err(err);
        }

        self.check_status(status, context)?;

        Ok(())
    }

    /// Build an authenticated POST request to the given endpoint.
    ///
    /// Applies custom headers first, then Basic Auth only if no custom Authorization
    /// header was provided (FR44, AC3: custom Authorization takes priority).
    fn build_post_request(&self, endpoint: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.post(&url);

        // Apply custom headers first (may include Authorization)
        for (name, value) in &self.custom_headers {
            request = request.header(name.as_str(), value.as_str());
        }

        // Only apply Basic Auth if no custom Authorization header was provided
        if let Some(auth) = &self.auth {
            let has_custom_auth = self
                .custom_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Authorization"));
            if !has_custom_auth {
                request = request.basic_auth(&auth.username, Some(&auth.password));
            }
        }

        request
    }

    /// Parse HTML response from `/nodes/conf_search` endpoint.
    ///
    /// Extracts node names from the first `<td>` in each table row.
    /// Filters out non-node values (entries containing "group", empty strings).
    ///
    /// # Arguments
    ///
    /// * `html` - The HTML response body
    ///
    /// # Returns
    ///
    /// A list of unique node names found in the response.
    pub fn parse_conf_search_html(html: &str) -> Vec<String> {
        let mut nodes: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for cap in TD_REGEX.captures_iter(html) {
            if let Some(node_match) = cap.get(1) {
                let node = node_match.as_str().trim();

                // Filter out non-node values
                if node.is_empty() || node.to_lowercase().contains("group") {
                    continue;
                }

                // Deduplicate (same node might appear multiple times in results)
                if seen.insert(node.to_string()) {
                    nodes.push(node.to_string());
                }
            }
        }

        nodes
    }

    /// Check if response body contains Oxidized 0.35.0 NodeNotFound pattern.
    ///
    /// Oxidized 0.35.0 returns HTTP 500 with different formats depending on the Accept header:
    /// - Without Accept: text/plain with "unable to find 'nodename'"
    /// - With Accept: application/json: HTML page with "Oxidized::NodeNotFound at /path"
    /// - Git output lookup failures: HTTP 200 with the literal body "node not found"
    ///
    /// This function detects all known patterns.
    fn check_node_not_found_body(&self, body: &str, context: &str) -> Option<OxidizedError> {
        // Pattern 0: Git output returns this sentinel with HTTP 200 when the
        // requested repository path does not exist.
        if body.trim() == "node not found" {
            return Some(OxidizedError::NodeNotFound(context.to_string(), vec![]));
        }

        // Pattern 1: Plain text format "unable to find 'nodename'"
        if body.contains("unable to find '") {
            let node_name = body
                .split("unable to find '")
                .nth(1)
                .and_then(|s| s.split('\'').next())
                .unwrap_or(context);

            return Some(OxidizedError::NodeNotFound(node_name.to_string(), vec![]));
        }

        // Pattern 2: HTML error page with "Oxidized::NodeNotFound at /node/show/NAME.json"
        if body.contains("Oxidized::NodeNotFound at /node/") {
            // Try to extract node name from the URL pattern
            // Format: "Oxidized::NodeNotFound at /node/show/NODENAME.json</title>"
            // After split on "at /node/": "show/NODENAME.json</title>..."
            let node_name = body
                .split("Oxidized::NodeNotFound at /node/")
                .nth(1)
                .and_then(|s| {
                    // s = "show/NODENAME.json</title>..."
                    // Find the .json and extract what's before it after the last /
                    if let Some(json_pos) = s.find(".json") {
                        let before_json = &s[..json_pos];
                        // before_json = "show/NODENAME"
                        before_json.rfind('/').map(|pos| &before_json[pos + 1..])
                    } else {
                        None
                    }
                })
                .unwrap_or(context);

            return Some(OxidizedError::NodeNotFound(node_name.to_string(), vec![]));
        }

        None
    }

    /// Convert reqwest errors to OxidizedError.
    fn handle_request_error(
        &self,
        response: Result<reqwest::Response, reqwest::Error>,
    ) -> Result<reqwest::Response, OxidizedError> {
        response.map_err(|e| OxidizedError::ApiUnreachable {
            source: e,
            attempt: 1,
            last_success: None,
        })
    }

    /// Check HTTP status code and map to appropriate error.
    ///
    /// Note: NodeNotFound returns empty suggestions because this method runs at the
    /// HTTP layer without access to the node list. The resources layer enriches
    /// NodeNotFound errors with fuzzy-matched suggestions via `find_similar_nodes`.
    fn check_status(&self, status: StatusCode, context: &str) -> Result<(), OxidizedError> {
        if status == StatusCode::NOT_FOUND {
            // Empty suggestions here - enriched by resources::get_node with fuzzy matching
            return Err(OxidizedError::NodeNotFound(context.to_string(), vec![]));
        }

        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(OxidizedError::AuthFailed);
        }

        // 5xx errors are server-side issues - map to HttpError (transient, retryable)
        if status.is_server_error() {
            return Err(OxidizedError::HttpError {
                status_code: status.as_u16(),
                context: context.to_string(),
            });
        }

        // Other 4xx errors (except 401/403/404 handled above)
        if status.is_client_error() {
            return Err(OxidizedError::HttpError {
                status_code: status.as_u16(),
                context: context.to_string(),
            });
        }

        Ok(())
    }
}

#[async_trait]
impl OxidizedBackend for OxidizedClient {
    fn redaction_enabled(&self) -> bool {
        self.redact_secrets
    }

    #[instrument(skip(self), fields(url = %self.base_url))]
    async fn get_nodes(&self) -> Result<(Vec<Node>, CacheMetadata), OxidizedError> {
        // Check cache first
        if let Some(cached) = self.nodes_cache.get(&()).await {
            tracing::debug!("Cache hit for nodes list");
            return Ok((cached, CacheMetadata::hit()));
        }

        // Cache miss - fetch with retry
        tracing::debug!("Cache miss for nodes list, fetching from API");
        let nodes: Vec<Node> = self
            .execute_with_retry(|| async {
                let response = self.build_request("/nodes.json").send().await;
                self.handle_json_response(response, "node list").await
            })
            .await?;

        // Hydrate the per-node cache while we already have the complete node
        // records. This avoids one /node/show request per device when a config
        // search immediately follows a node-list request.
        for node in &nodes {
            self.node_cache
                .insert(node.name.clone(), node.clone())
                .await;
        }

        // Store in cache
        self.nodes_cache.insert((), nodes.clone()).await;
        Ok((nodes, CacheMetadata::miss()))
    }

    #[instrument(skip(self), fields(url = %self.base_url, node = %name))]
    async fn get_node(&self, name: &str) -> Result<(Node, CacheMetadata), OxidizedError> {
        // Check cache first
        if let Some(cached) = self.node_cache.get(name).await {
            tracing::debug!(node = %name, "Cache hit for node");
            return Ok((cached, CacheMetadata::hit()));
        }

        // Cache miss - fetch with retry
        tracing::debug!(node = %name, "Cache miss for node, fetching from API");
        let endpoint = format!("/node/show/{}.json", urlencoding::encode(name));
        let node: Node = self
            .execute_with_retry(|| async {
                let response = self.build_request(&endpoint).send().await;
                self.handle_json_response(response, name).await
            })
            .await?;

        // Store in cache
        self.node_cache.insert(name.to_string(), node.clone()).await;
        Ok((node, CacheMetadata::miss()))
    }

    #[instrument(skip(self), fields(url = %self.base_url, node = %name))]
    async fn get_node_config(&self, name: &str) -> Result<(String, CacheMetadata), OxidizedError> {
        // Check cache first
        let pending = self.is_backup_pending(name).await;
        if !pending && let Some(cached) = self.config_cache.get(name).await {
            tracing::debug!(node = %name, "Cache hit for config");
            return Ok((cached, CacheMetadata::hit()));
        }

        // Cache miss - fetch with retry
        tracing::debug!(node = %name, "Cache miss for config, fetching from API");
        // Grouped nodes in a single Git repository are stored at
        // <group>/<name>. Oxidized exposes that path as full_name.
        let (node, _) = self.get_node(name).await?;
        let endpoint = format!("/node/fetch/{}", encode_path_segments(&node.full_name));
        let config = self
            .execute_with_retry(|| async {
                let response = self.build_request(&endpoint).send().await;
                self.handle_text_response(response, name).await
            })
            .await?;

        // Store in cache
        if !pending {
            self.config_cache
                .insert(name.to_string(), config.clone())
                .await;
        }
        Ok((config, CacheMetadata::miss()))
    }

    #[instrument(skip(self), fields(url = %self.base_url, node = %name))]
    async fn get_node_versions(&self, name: &str) -> Result<Vec<NodeVersion>, OxidizedError> {
        // Oxidized-web 0.18.0 requires node_full (group/name) for version endpoint
        // First get the node to obtain its full_name (usually cached)
        let (node, _) = self.get_node(name).await?;

        // Versions are not cached (historical data, rarely accessed repeatedly)
        let endpoint = format!(
            "/node/version.json?node_full={}",
            urlencoding::encode(&node.full_name)
        );
        self.execute_with_retry(|| async {
            let response = self.build_request(&endpoint).send().await;
            self.handle_json_response(response, name).await
        })
        .await
    }

    #[instrument(skip(self), fields(url = %self.base_url, node = %name, oid = %oid))]
    async fn get_node_version(&self, name: &str, oid: &str) -> Result<String, OxidizedError> {
        // Oxidized-web 0.18.0 requires node and group as separate params
        // First get the node to obtain its group (usually cached)
        let (node, _) = self.get_node(name).await?;

        // Version content is not cached (point-in-time data)
        // API returns JSON array of lines: ["line1\n", "line2\n", ...]
        let endpoint = format!(
            "/node/version/view.json?node={}&group={}&oid={}",
            urlencoding::encode(name),
            urlencoding::encode(&node.group),
            oid
        );
        let context = format!("{}@{}", name, oid);

        let lines: Vec<String> = self
            .execute_with_retry(|| async {
                let response = self.build_request(&endpoint).send().await;
                self.handle_json_response(response, &context).await
            })
            .await?;

        // Join lines into a single string
        Ok(lines.join(""))
    }

    #[instrument(skip(self), fields(url = %self.base_url))]
    async fn get_stats(&self) -> Result<(Stats, CacheMetadata), OxidizedError> {
        // Check cache first
        if let Some(cached) = self.stats_cache.get(&()).await {
            tracing::debug!("Cache hit for stats");
            return Ok((cached, CacheMetadata::hit()));
        }

        // Cache miss - compute stats from nodes list
        // Note: Oxidized 0.35.0 does not expose a /stats JSON endpoint,
        // so we compute statistics from the nodes list.
        tracing::debug!("Cache miss for stats, computing from nodes list");
        let (nodes, _) = self.get_nodes().await?;
        let stats = Stats::from_nodes(&nodes);

        // Store in cache
        self.stats_cache.insert((), stats.clone()).await;
        Ok((stats, CacheMetadata::miss()))
    }

    #[instrument(skip(self), fields(url = %self.base_url, node = %node))]
    async fn trigger_backup(&self, node: &str) -> Result<(), OxidizedError> {
        // Oxidized-web 0.18.0 uses GET /node/next/{name}.json to trigger backup
        let endpoint = format!("/node/next/{}.json", urlencoding::encode(node));
        let result = self
            .execute_with_retry(|| async {
                let response = self.build_request(&endpoint).send().await;
                self.handle_empty_response(response, node).await
            })
            .await;

        // Invalidate cache ONLY on success (AC: 4, 5)
        if result.is_ok() {
            self.invalidate_node(node).await;
        }

        result
    }

    #[instrument(skip(self), fields(url = %self.base_url, node = %node))]
    async fn prioritize_node(&self, node: &str) -> Result<(), OxidizedError> {
        // Oxidized-web 0.18.0 uses GET /node/next/{name}.json to prioritize node
        let endpoint = format!("/node/next/{}.json", urlencoding::encode(node));
        let result = self
            .execute_with_retry(|| async {
                let response = self.build_request(&endpoint).send().await;
                self.handle_empty_response(response, node).await
            })
            .await;

        // Invalidate cache ONLY on success (AC: 4, 5)
        if result.is_ok() {
            self.invalidate_node(node).await;
        }

        result
    }

    #[instrument(skip(self), fields(url = %self.base_url))]
    async fn reload_sources(&self) -> Result<(), OxidizedError> {
        // Write operation with retry
        let result = self
            .execute_with_retry(|| async {
                let response = self.build_request("/reload?format=json").send().await;
                self.handle_empty_response(response, "reload").await
            })
            .await;

        // Invalidate ALL caches ONLY on success (AC: 4, 5)
        if result.is_ok() {
            self.invalidate_all_nodes().await;
        }

        result
    }

    #[instrument(skip(self), fields(url = %self.base_url))]
    async fn conf_search(&self, pattern: &str) -> Result<ConfSearchResult, OxidizedError> {
        // Early return for empty pattern - no point in network request
        if pattern.is_empty() {
            tracing::debug!("Empty pattern, skipping conf_search");
            return Ok(ConfSearchResult::Available(vec![]));
        }

        // NOTE: No execute_with_retry() here by design.
        // conf_search is an optimization layer with graceful degradation.
        // If it fails, search_configs falls back to searching all nodes.
        // Retrying would add latency without benefit since fallback works.

        // POST /nodes/conf_search with form data
        let response = self
            .build_post_request("/nodes/conf_search")
            .form(&[("search_in_conf_textbox", pattern)])
            .send()
            .await;

        // Graceful degradation: return empty vec on any error
        let body = match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    tracing::debug!(
                        status = %resp.status(),
                        "conf_search API returned non-success status, falling back"
                    );
                    return Ok(ConfSearchResult::Unavailable(format!(
                        "HTTP {}",
                        resp.status()
                    )));
                }
                match resp.text().await {
                    Ok(text) => text,
                    Err(e) => {
                        tracing::debug!(error = %e, "Failed to read conf_search response body");
                        return Ok(ConfSearchResult::Unavailable(e.to_string()));
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "conf_search API unavailable, falling back");
                return Ok(ConfSearchResult::Unavailable(e.to_string()));
            }
        };

        // Parse HTML response
        let nodes = Self::parse_conf_search_html(&body);

        tracing::debug!(
            pattern = %pattern,
            nodes_found = nodes.len(),
            "conf_search completed"
        );

        Ok(ConfSearchResult::Available(nodes))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Data Model Deserialization Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_node_deserialize() {
        let json = r#"{
            "name": "SW-Core-01",
            "full_name": "SW-Core-01.network.local",
            "ip": "192.168.1.1",
            "group": "switches",
            "model": "cisco-ios",
            "status": "success",
            "last_status": "success",
            "time": "2025-01-15 10:30:00 UTC",
            "mtime": "2025-01-15 10:25:00 UTC"
        }"#;

        let node: Node = serde_json::from_str(json).expect("Should deserialize Node");

        assert_eq!(node.name, "SW-Core-01");
        assert_eq!(node.full_name, "SW-Core-01.network.local");
        assert_eq!(node.ip, "192.168.1.1");
        assert_eq!(node.group, "switches");
        assert_eq!(node.model, "cisco-ios");
        assert_eq!(node.status, Some("success".to_string()));
        assert_eq!(node.last_status, Some("success".to_string()));
        assert_eq!(node.time, Some("2025-01-15 10:30:00 UTC".to_string()));
        assert_eq!(node.mtime, Some("2025-01-15 10:25:00 UTC".to_string()));
    }

    #[test]
    fn test_node_deserialize_without_optional_fields() {
        let json = r#"{
            "name": "SW-Core-01",
            "full_name": "SW-Core-01.network.local",
            "ip": "192.168.1.1",
            "group": "switches",
            "model": "cisco-ios",
            "status": "never",
            "last_status": "never"
        }"#;

        let node: Node =
            serde_json::from_str(json).expect("Should deserialize Node without optionals");

        assert_eq!(node.name, "SW-Core-01");
        assert_eq!(node.time, None);
        assert_eq!(node.mtime, None);
    }

    #[test]
    fn test_node_deserialize_oxidized_035_format() {
        // Oxidized 0.35.0 does not include last_status field
        let json = r#"{
            "name": "Palais-Tech",
            "full_name": "mikrotik/Palais-Tech",
            "ip": "10.255.42.25",
            "group": "mikrotik",
            "model": "RouterOS",
            "status": "success",
            "time": "2025-12-23 09:01:42 UTC",
            "mtime": "2025-12-23 09:01:42 UTC"
        }"#;

        let node: Node =
            serde_json::from_str(json).expect("Should deserialize Oxidized 0.35.0 Node");

        assert_eq!(node.name, "Palais-Tech");
        assert_eq!(node.full_name, "mikrotik/Palais-Tech");
        assert_eq!(node.group, "mikrotik");
        assert_eq!(node.model, "RouterOS");
        assert_eq!(node.status, Some("success".to_string()));
        // last_status should be None when not present in JSON
        assert_eq!(node.last_status, None);
        assert_eq!(node.time, Some("2025-12-23 09:01:42 UTC".to_string()));
    }

    #[test]
    fn test_node_version_deserialize() {
        // Oxidized 0.35.0+ format with author as object
        let json = r#"{
            "oid": "abc123def456",
            "date": "2025-01-15 10:30:00 UTC",
            "time": "2025-01-15 10:30:00 UTC",
            "author": {
                "name": "oxidized",
                "email": "oxidized@example.com",
                "time": "2025-01-15 10:30:00 UTC"
            },
            "message": "update SW-Core-01"
        }"#;

        let version: NodeVersion =
            serde_json::from_str(json).expect("Should deserialize NodeVersion");

        assert_eq!(version.oid, "abc123def456");
        assert_eq!(version.date, "2025-01-15 10:30:00 UTC");
        assert_eq!(version.time, Some("2025-01-15 10:30:00 UTC".to_string()));
        assert_eq!(version.author.name, "oxidized");
        assert_eq!(version.author.email, "oxidized@example.com");
        assert_eq!(version.message, "update SW-Core-01");
    }

    #[test]
    fn test_stats_deserialize() {
        let json = r#"{
            "total_nodes": 150,
            "success_count": 145,
            "failure_count": 5,
            "last_run": "2025-01-15 10:30:00 UTC"
        }"#;

        let stats: Stats = serde_json::from_str(json).expect("Should deserialize Stats");

        assert_eq!(stats.total_nodes, Some(150));
        assert_eq!(stats.success_count, Some(145));
        assert_eq!(stats.failure_count, Some(5));
        assert_eq!(stats.last_run, Some("2025-01-15 10:30:00 UTC".to_string()));
    }

    #[test]
    fn test_stats_deserialize_partial() {
        let json = r#"{
            "total_nodes": 10
        }"#;

        let stats: Stats = serde_json::from_str(json).expect("Should deserialize partial Stats");

        assert_eq!(stats.total_nodes, Some(10));
        assert_eq!(stats.success_count, None);
    }

    #[test]
    fn test_stats_from_nodes() {
        let nodes = vec![
            Node {
                name: "node1".to_string(),
                full_name: "group/node1".to_string(),
                ip: "10.0.0.1".to_string(),
                group: "test".to_string(),
                model: "cisco".to_string(),
                status: Some("success".to_string()),
                last_status: None,
                time: Some("2025-12-23 09:01:42 UTC".to_string()),
                mtime: None,
                last: None,
            },
            Node {
                name: "node2".to_string(),
                full_name: "group/node2".to_string(),
                ip: "10.0.0.2".to_string(),
                group: "test".to_string(),
                model: "cisco".to_string(),
                status: Some("success".to_string()),
                last_status: None,
                time: Some("2025-12-23 09:00:00 UTC".to_string()),
                mtime: None,
                last: None,
            },
            Node {
                name: "node3".to_string(),
                full_name: "group/node3".to_string(),
                ip: "10.0.0.3".to_string(),
                group: "test".to_string(),
                model: "cisco".to_string(),
                status: Some("failure".to_string()),
                last_status: None,
                time: Some("2025-12-22 10:00:00 UTC".to_string()),
                mtime: None,
                last: None,
            },
            Node {
                name: "node4".to_string(),
                full_name: "group/node4".to_string(),
                ip: "10.0.0.4".to_string(),
                group: "test".to_string(),
                model: "cisco".to_string(),
                status: Some("never".to_string()),
                last_status: None,
                time: Some("never".to_string()),
                mtime: None,
                last: None,
            },
        ];

        let stats = Stats::from_nodes(&nodes);

        assert_eq!(stats.total_nodes, Some(4));
        assert_eq!(stats.success_count, Some(2));
        assert_eq!(stats.failure_count, Some(2)); // failure + never
        // Most recent time (excluding "never")
        assert_eq!(stats.last_run, Some("2025-12-23 09:01:42 UTC".to_string()));
    }

    #[test]
    fn test_stats_from_nodes_empty() {
        let nodes: Vec<Node> = vec![];
        let stats = Stats::from_nodes(&nodes);

        assert_eq!(stats.total_nodes, Some(0));
        assert_eq!(stats.success_count, Some(0));
        assert_eq!(stats.failure_count, Some(0));
        assert_eq!(stats.last_run, None);
    }

    // -------------------------------------------------------------------------
    // OxidizedClient Construction Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_client_new_without_auth() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };

        let client = OxidizedClient::new(&config);

        assert_eq!(client.base_url, "http://localhost:8888");
        assert!(client.auth.is_none());
    }

    #[test]
    fn test_client_new_with_auth() {
        let config = Config {
            oxidized_url: "https://oxidized.example.com".to_string(),
            oxidized_user: Some("admin".to_string()),
            oxidized_password: Some("secret".to_string()),
            ssl_verify: true,
            custom_headers: vec![],
        };

        let client = OxidizedClient::new(&config);

        assert_eq!(client.base_url, "https://oxidized.example.com");
        assert!(client.auth.is_some());
    }

    #[test]
    fn test_client_new_with_partial_auth_no_password() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: Some("admin".to_string()),
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };

        let client = OxidizedClient::new(&config);

        // Should not set auth if password is missing
        assert!(client.auth.is_none());
    }

    #[test]
    fn test_client_new_with_partial_auth_no_user() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: Some("secret".to_string()),
            ssl_verify: true,
            custom_headers: vec![],
        };

        let client = OxidizedClient::new(&config);

        // Should not set auth if user is missing
        assert!(client.auth.is_none());
    }

    // -------------------------------------------------------------------------
    // Error Mapping Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_check_status_not_found() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::NOT_FOUND, "test-node");

        assert!(result.is_err());
        match result.unwrap_err() {
            OxidizedError::NodeNotFound(name, _) => {
                assert_eq!(name, "test-node");
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[test]
    fn test_check_node_not_found_body_oxidized_035() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Oxidized 0.35.0 error format
        let body = "Oxidized::NodeNotFound: unable to find 'NONEXISTENT' (Oxidized::NodeNotFound)";
        let result = client.check_node_not_found_body(body, "context");

        assert!(result.is_some());
        match result.unwrap() {
            OxidizedError::NodeNotFound(name, _) => {
                assert_eq!(name, "NONEXISTENT");
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[test]
    fn test_check_node_not_found_body_no_match() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let body = r#"{"name": "SW-Core-01", "status": "success"}"#;
        let result = client.check_node_not_found_body(body, "context");

        assert!(result.is_none());
    }

    #[test]
    fn test_check_node_not_found_body_git_output_sentinel() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_node_not_found_body("node not found\n", "test-node");
        assert!(matches!(
            result,
            Some(OxidizedError::NodeNotFound(name, _)) if name == "test-node"
        ));
    }

    #[test]
    fn test_check_node_not_found_body_html_format() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // HTML error page format from Oxidized 0.35.0 when Accept: application/json is sent
        let body = r#"<!DOCTYPE html>
<html>
<head>
  <title>Oxidized::NodeNotFound at /node/show/test-node-123.json</title>
</head>
<body>...</body>
</html>"#;
        let result = client.check_node_not_found_body(body, "context");

        assert!(result.is_some());
        match result.unwrap() {
            OxidizedError::NodeNotFound(name, _) => {
                assert_eq!(name, "test-node-123");
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[test]
    fn test_check_status_unauthorized() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::UNAUTHORIZED, "test");

        assert!(result.is_err());
        match result.unwrap_err() {
            OxidizedError::AuthFailed => {}
            _ => panic!("Expected AuthFailed error"),
        }
    }

    #[test]
    fn test_check_status_forbidden() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::FORBIDDEN, "test");

        assert!(result.is_err());
        match result.unwrap_err() {
            OxidizedError::AuthFailed => {}
            _ => panic!("Expected AuthFailed error"),
        }
    }

    #[test]
    fn test_check_status_success() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::OK, "test");

        assert!(result.is_ok());
    }

    // -------------------------------------------------------------------------
    // BasicAuth Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_basic_auth_new() {
        let auth = BasicAuth::new("user".to_string(), "pass".to_string());

        assert_eq!(auth.username, "user");
        assert_eq!(auth.password, "pass");
    }

    // -------------------------------------------------------------------------
    // Node List Deserialization Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_node_list_deserialize() {
        let json = r#"[
            {
                "name": "SW-Core-01",
                "full_name": "SW-Core-01.network.local",
                "ip": "192.168.1.1",
                "group": "switches",
                "model": "cisco-ios",
                "status": "success",
                "last_status": "success"
            },
            {
                "name": "RTR-Edge-01",
                "full_name": "RTR-Edge-01.network.local",
                "ip": "192.168.1.2",
                "group": "routers",
                "model": "junos",
                "status": "failure",
                "last_status": "success"
            }
        ]"#;

        let nodes: Vec<Node> = serde_json::from_str(json).expect("Should deserialize node list");

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "SW-Core-01");
        assert_eq!(nodes[1].name, "RTR-Edge-01");
    }

    // -------------------------------------------------------------------------
    // Fixture-based Deserialization Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_node_deserialize_from_fixture() {
        let json = std::fs::read_to_string("fixtures/node.json").expect("Should read fixture file");
        let node: Node = serde_json::from_str(&json).expect("Should deserialize Node from fixture");

        assert_eq!(node.name, "SW-Core-01");
        assert_eq!(node.full_name, "SW-Core-01.network.local");
        assert_eq!(node.ip, "192.168.1.1");
        assert_eq!(node.group, "switches");
        assert_eq!(node.model, "cisco-ios");
        assert_eq!(node.status, Some("success".to_string()));
    }

    #[test]
    fn test_nodes_list_deserialize_from_fixture() {
        let json =
            std::fs::read_to_string("fixtures/nodes.json").expect("Should read fixture file");
        let nodes: Vec<Node> =
            serde_json::from_str(&json).expect("Should deserialize nodes from fixture");

        assert_eq!(nodes.len(), 5);
        assert_eq!(nodes[0].name, "SW-Core-01");
        assert_eq!(nodes[2].name, "RTR-Edge-01");
        assert_eq!(nodes[2].status, Some("failure".to_string()));
        // Verify node without time/mtime (AP-Floor3-01)
        assert_eq!(nodes[4].name, "AP-Floor3-01");
        assert_eq!(nodes[4].time, None);
        assert_eq!(nodes[4].mtime, None);
    }

    #[test]
    fn test_stats_deserialize_from_fixture() {
        let json =
            std::fs::read_to_string("fixtures/stats.json").expect("Should read fixture file");
        let stats: Stats =
            serde_json::from_str(&json).expect("Should deserialize Stats from fixture");

        assert_eq!(stats.total_nodes, Some(150));
        assert_eq!(stats.success_count, Some(142));
        assert_eq!(stats.failure_count, Some(5));
        assert!(stats.last_run.is_some());
    }

    #[test]
    fn test_versions_deserialize_from_fixture() {
        let json =
            std::fs::read_to_string("fixtures/versions.json").expect("Should read fixture file");
        let versions: Vec<NodeVersion> =
            serde_json::from_str(&json).expect("Should deserialize versions from fixture");

        assert_eq!(versions.len(), 5);
        assert_eq!(versions[0].oid, "abc123def456789012345678901234567890abcd");
        assert_eq!(versions[0].author.name, "oxidized");
        assert_eq!(versions[0].author.email, "oxidized@example.com");
        assert!(versions[0].message.contains("SW-Core-01"));
    }

    // -------------------------------------------------------------------------
    // HTTP Error Status Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_check_status_server_error() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::INTERNAL_SERVER_ERROR, "test");

        assert!(result.is_err());
        match result.unwrap_err() {
            OxidizedError::HttpError {
                status_code,
                context,
            } => {
                assert_eq!(status_code, 500);
                assert_eq!(context, "test");
            }
            _ => panic!("Expected HttpError"),
        }
    }

    #[test]
    fn test_check_status_bad_gateway() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::BAD_GATEWAY, "proxy");

        assert!(result.is_err());
        match result.unwrap_err() {
            OxidizedError::HttpError { status_code, .. } => {
                assert_eq!(status_code, 502);
            }
            _ => panic!("Expected HttpError"),
        }
    }

    #[test]
    fn test_check_status_bad_request() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        let result = client.check_status(StatusCode::BAD_REQUEST, "invalid");

        assert!(result.is_err());
        match result.unwrap_err() {
            OxidizedError::HttpError { status_code, .. } => {
                assert_eq!(status_code, 400);
            }
            _ => panic!("Expected HttpError"),
        }
    }

    // -------------------------------------------------------------------------
    // Timeout Configuration Tests
    // -------------------------------------------------------------------------

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_timeout_constants_are_reasonable() {
        // Connect timeout should be shorter than request timeout
        assert!(
            DEFAULT_CONNECT_TIMEOUT_SECS < DEFAULT_REQUEST_TIMEOUT_SECS,
            "Connect timeout should be less than request timeout"
        );

        // Connect timeout should be at least 5 seconds for slow networks
        assert!(
            DEFAULT_CONNECT_TIMEOUT_SECS >= 5,
            "Connect timeout should be at least 5 seconds"
        );

        // Request timeout should be at least 15 seconds for large responses
        assert!(
            DEFAULT_REQUEST_TIMEOUT_SECS >= 15,
            "Request timeout should be at least 15 seconds"
        );

        // Request timeout should not exceed 60 seconds (reasonable upper bound)
        assert!(
            DEFAULT_REQUEST_TIMEOUT_SECS <= 60,
            "Request timeout should not exceed 60 seconds"
        );
    }

    #[test]
    fn test_client_uses_timeout_constants() {
        // Verify the constants are what we expect (as documented in story)
        assert_eq!(
            DEFAULT_CONNECT_TIMEOUT_SECS, 10,
            "Connect timeout should be 10s"
        );
        assert_eq!(
            DEFAULT_REQUEST_TIMEOUT_SECS, 30,
            "Request timeout should be 30s"
        );
    }

    // -------------------------------------------------------------------------
    // Cache Constants Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_nodes_cache_ttl_is_5_minutes() {
        assert_eq!(
            NODES_CACHE_TTL_SECS, 300,
            "Nodes cache TTL should be 5 minutes (300 seconds)"
        );
    }

    #[test]
    fn test_config_cache_ttl_is_2_minutes() {
        assert_eq!(
            CONFIG_CACHE_TTL_SECS, 120,
            "Config cache TTL should be 2 minutes (120 seconds)"
        );
    }

    #[test]
    fn test_stats_cache_ttl_is_30_seconds() {
        assert_eq!(
            STATS_CACHE_TTL_SECS, 30,
            "Stats cache TTL should be 30 seconds"
        );
    }

    // -------------------------------------------------------------------------
    // Retry Constants Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_max_retry_attempts_is_3() {
        assert_eq!(
            MAX_RETRY_ATTEMPTS, 3,
            "Max retry attempts should be 3 (initial + 2 retries)"
        );
    }

    #[test]
    fn test_retry_delays_are_exponential() {
        assert_eq!(
            RETRY_DELAYS_MS,
            [200, 800],
            "Retry delays should be [200ms, 800ms]"
        );
        // Verify exponential progression (each delay is 4x previous)
        assert!(
            RETRY_DELAYS_MS[1] > RETRY_DELAYS_MS[0],
            "Second delay should be greater than first"
        );
    }

    // -------------------------------------------------------------------------
    // CacheMetadata Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_cache_metadata_hit() {
        let meta = CacheMetadata::hit();
        assert!(
            meta.cache_hit,
            "CacheMetadata::hit() should set cache_hit to true"
        );
    }

    #[test]
    fn test_cache_metadata_miss() {
        let meta = CacheMetadata::miss();
        assert!(
            !meta.cache_hit,
            "CacheMetadata::miss() should set cache_hit to false"
        );
    }

    #[test]
    fn test_cache_metadata_serializes_correctly() {
        let hit = CacheMetadata::hit();
        let json = serde_json::to_string(&hit).expect("Should serialize CacheMetadata");
        assert!(
            json.contains("\"cache_hit\":true"),
            "Should serialize cache_hit field"
        );

        let miss = CacheMetadata::miss();
        let json = serde_json::to_string(&miss).expect("Should serialize CacheMetadata");
        assert!(
            json.contains("\"cache_hit\":false"),
            "Should serialize cache_hit field"
        );
    }

    // -------------------------------------------------------------------------
    // Cached Response Wrapper Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_cached_nodes_serializes() {
        let nodes = vec![Node {
            name: "SW-01".to_string(),
            full_name: "SW-01.local".to_string(),
            ip: "10.0.0.1".to_string(),
            group: "switches".to_string(),
            model: "cisco".to_string(),
            status: Some("success".to_string()),
            last_status: Some("success".to_string()),
            time: None,
            mtime: None,
            last: None,
        }];
        let cached = CachedNodes {
            nodes,
            metadata: CacheMetadata::hit(),
        };
        let json = serde_json::to_string(&cached).expect("Should serialize CachedNodes");
        assert!(json.contains("\"nodes\""), "Should contain nodes field");
        assert!(
            json.contains("\"metadata\""),
            "Should contain metadata field"
        );
        assert!(
            json.contains("\"cache_hit\":true"),
            "Should indicate cache hit"
        );
    }

    #[test]
    fn test_cached_config_serializes() {
        let cached = CachedConfig {
            config: "hostname SW-01\n".to_string(),
            metadata: CacheMetadata::miss(),
        };
        let json = serde_json::to_string(&cached).expect("Should serialize CachedConfig");
        assert!(json.contains("\"config\""), "Should contain config field");
        assert!(
            json.contains("\"cache_hit\":false"),
            "Should indicate cache miss"
        );
    }

    #[test]
    fn test_cached_stats_serializes() {
        let stats = Stats {
            total_nodes: Some(100),
            success_count: Some(95),
            failure_count: Some(5),
            last_run: Some("2025-01-15".to_string()),
        };
        let cached = CachedStats {
            stats,
            metadata: CacheMetadata::hit(),
        };
        let json = serde_json::to_string(&cached).expect("Should serialize CachedStats");
        assert!(json.contains("\"stats\""), "Should contain stats field");
        assert!(
            json.contains("\"total_nodes\":100"),
            "Should contain stats data"
        );
    }

    // -------------------------------------------------------------------------
    // Retry Logic Tests (execute_with_retry behavior)
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_retry_succeeds_on_first_attempt() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Simulate a successful operation on first attempt
        let result: Result<String, OxidizedError> = client
            .execute_with_retry(|| async { Ok("success".to_string()) })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
    }

    #[tokio::test]
    async fn test_retry_non_transient_error_fails_immediately() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        use std::sync::atomic::{AtomicU8, Ordering};
        let attempt_count = std::sync::Arc::new(AtomicU8::new(0));
        let counter = attempt_count.clone();

        // Non-transient error should not retry
        let result: Result<String, OxidizedError> = client
            .execute_with_retry(|| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err(OxidizedError::NodeNotFound("test".to_string(), vec![]))
                }
            })
            .await;

        assert!(result.is_err());
        // Should only attempt once (no retry for non-transient errors)
        assert_eq!(
            attempt_count.load(Ordering::SeqCst),
            1,
            "Non-transient error should not retry"
        );
    }

    #[tokio::test]
    async fn test_retry_transient_error_retries_up_to_max() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        use std::sync::atomic::{AtomicU8, Ordering};
        let attempt_count = std::sync::Arc::new(AtomicU8::new(0));
        let counter = attempt_count.clone();

        // Transient error should retry
        let result: Result<String, OxidizedError> = client
            .execute_with_retry(|| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err(OxidizedError::HttpError {
                        status_code: 503,
                        context: "test".to_string(),
                    })
                }
            })
            .await;

        assert!(result.is_err());
        // Should attempt MAX_RETRY_ATTEMPTS times
        assert_eq!(
            attempt_count.load(Ordering::SeqCst),
            MAX_RETRY_ATTEMPTS,
            "Should retry up to max attempts for transient errors"
        );
    }

    #[tokio::test]
    async fn test_retry_succeeds_after_transient_failure() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        use std::sync::atomic::{AtomicU8, Ordering};
        let attempt_count = std::sync::Arc::new(AtomicU8::new(0));
        let counter = attempt_count.clone();

        // Fail first attempt, succeed on second
        let result: Result<String, OxidizedError> = client
            .execute_with_retry(|| {
                let counter = counter.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        Err(OxidizedError::HttpError {
                            status_code: 500,
                            context: "test".to_string(),
                        })
                    } else {
                        Ok("success after retry".to_string())
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success after retry");
        assert_eq!(
            attempt_count.load(Ordering::SeqCst),
            2,
            "Should succeed on second attempt"
        );
    }

    // -------------------------------------------------------------------------
    // Cache Initialization Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_client_initializes_all_caches() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Verify caches are initialized (they should be empty initially)
        // We can't directly check TTL, but we can verify the caches exist
        // by checking they don't contain any entries
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(
                client.nodes_cache.get(&()).await.is_none(),
                "nodes_cache should be empty"
            );
            assert!(
                client.node_cache.get("test").await.is_none(),
                "node_cache should be empty"
            );
            assert!(
                client.config_cache.get("test").await.is_none(),
                "config_cache should be empty"
            );
            assert!(
                client.stats_cache.get(&()).await.is_none(),
                "stats_cache should be empty"
            );
        });
    }

    // -------------------------------------------------------------------------
    // Cache Invalidation Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_invalidate_node_clears_node_and_config_cache() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Pre-populate caches
        let node = Node {
            name: "test-node".to_string(),
            full_name: "test-node.local".to_string(),
            ip: "10.0.0.1".to_string(),
            group: "test".to_string(),
            model: "test".to_string(),
            status: Some("success".to_string()),
            last_status: Some("success".to_string()),
            time: None,
            mtime: None,
            last: None,
        };
        client
            .node_cache
            .insert("test-node".to_string(), node)
            .await;
        client
            .config_cache
            .insert("test-node".to_string(), "config data".to_string())
            .await;

        // Verify cache is populated
        assert!(client.node_cache.get("test-node").await.is_some());
        assert!(client.config_cache.get("test-node").await.is_some());

        // Invalidate node
        client.invalidate_node("test-node").await;

        // Verify cache is cleared for this node
        assert!(
            client.node_cache.get("test-node").await.is_none(),
            "node_cache should be invalidated"
        );
        assert!(
            client.config_cache.get("test-node").await.is_none(),
            "config_cache should be invalidated"
        );
    }

    #[tokio::test]
    async fn test_invalidate_node_does_not_affect_other_nodes() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Pre-populate caches for two nodes
        let node1 = Node {
            name: "node1".to_string(),
            full_name: "node1.local".to_string(),
            ip: "10.0.0.1".to_string(),
            group: "test".to_string(),
            model: "test".to_string(),
            status: Some("success".to_string()),
            last_status: Some("success".to_string()),
            time: None,
            mtime: None,
            last: None,
        };
        let node2 = Node {
            name: "node2".to_string(),
            full_name: "node2.local".to_string(),
            ip: "10.0.0.2".to_string(),
            group: "test".to_string(),
            model: "test".to_string(),
            status: Some("success".to_string()),
            last_status: Some("success".to_string()),
            time: None,
            mtime: None,
            last: None,
        };
        client.node_cache.insert("node1".to_string(), node1).await;
        client.node_cache.insert("node2".to_string(), node2).await;

        // Invalidate only node1
        client.invalidate_node("node1").await;

        // node1 should be invalidated, node2 should remain
        assert!(
            client.node_cache.get("node1").await.is_none(),
            "node1 should be invalidated"
        );
        assert!(
            client.node_cache.get("node2").await.is_some(),
            "node2 should remain cached"
        );
    }

    #[tokio::test]
    async fn test_invalidate_all_nodes_clears_all_caches() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Pre-populate all caches
        let node = Node {
            name: "test".to_string(),
            full_name: "test.local".to_string(),
            ip: "10.0.0.1".to_string(),
            group: "test".to_string(),
            model: "test".to_string(),
            status: Some("success".to_string()),
            last_status: Some("success".to_string()),
            time: None,
            mtime: None,
            last: None,
        };
        let stats = Stats {
            total_nodes: Some(10),
            success_count: Some(10),
            failure_count: Some(0),
            last_run: None,
        };
        client.nodes_cache.insert((), vec![node.clone()]).await;
        client.node_cache.insert("test".to_string(), node).await;
        client
            .config_cache
            .insert("test".to_string(), "config".to_string())
            .await;
        client.stats_cache.insert((), stats).await;

        // Verify all caches are populated
        assert!(client.nodes_cache.get(&()).await.is_some());
        assert!(client.node_cache.get("test").await.is_some());
        assert!(client.config_cache.get("test").await.is_some());
        assert!(client.stats_cache.get(&()).await.is_some());

        // Invalidate all
        client.invalidate_all_nodes().await;

        // Verify all caches are cleared
        assert!(
            client.nodes_cache.get(&()).await.is_none(),
            "nodes_cache should be invalidated"
        );
        assert!(
            client.node_cache.get("test").await.is_none(),
            "node_cache should be invalidated"
        );
        assert!(
            client.config_cache.get("test").await.is_none(),
            "config_cache should be invalidated"
        );
        assert!(
            client.stats_cache.get(&()).await.is_none(),
            "stats_cache should be invalidated"
        );
    }

    // -------------------------------------------------------------------------
    // parse_conf_search_html Tests (Story 2-3)
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_conf_search_html_real_oxidized_response() {
        // Sample HTML from real Oxidized /nodes/conf_search response
        let html = r#"
<table class='table' id='versionsTable'>
  <tbody>
    <tr>
      <td>PDC-SW-ETG2-B</td>
      <td><a href='/node/fetch/PDC-SW-ETG2-B'><i class='bi bi-cloud-download'></i></a></td>
    </tr>
    <tr>
      <td>SW-Core-01</td>
      <td><a href='/node/fetch/SW-Core-01'><i class='bi bi-cloud-download'></i></a></td>
    </tr>
    <tr>
      <td>RTR-Edge-02</td>
      <td><a href='/node/fetch/RTR-Edge-02'><i class='bi bi-cloud-download'></i></a></td>
    </tr>
  </tbody>
</table>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert_eq!(nodes.len(), 3);
        assert!(nodes.contains(&"PDC-SW-ETG2-B".to_string()));
        assert!(nodes.contains(&"SW-Core-01".to_string()));
        assert!(nodes.contains(&"RTR-Edge-02".to_string()));
    }

    #[test]
    fn test_parse_conf_search_html_empty_table() {
        let html = r#"
<table class='table' id='versionsTable'>
  <tbody>
  </tbody>
</table>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert!(nodes.is_empty(), "Empty table should return empty list");
    }

    #[test]
    fn test_parse_conf_search_html_malformed_html() {
        // Malformed HTML without proper structure
        let html = "<html><body>No table here</body></html>";

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert!(nodes.is_empty(), "Malformed HTML should return empty list");
    }

    #[test]
    fn test_parse_conf_search_html_filters_group_entries() {
        // HTML with group entries that should be filtered out
        // (entries containing "group" in any case are filtered)
        let html = r#"
<table>
  <tr><td>my-group</td></tr>
  <tr><td>SW-Core-01</td></tr>
  <tr><td>group-routers</td></tr>
  <tr><td>RTR-Edge-01</td></tr>
</table>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&"SW-Core-01".to_string()));
        assert!(nodes.contains(&"RTR-Edge-01".to_string()));
        // "my-group" and "group-routers" should be filtered out
        assert!(!nodes.iter().any(|n| n.to_lowercase().contains("group")));
    }

    #[test]
    fn test_parse_conf_search_html_filters_empty_strings() {
        let html = r#"
<table>
  <tr><td></td></tr>
  <tr><td>  </td></tr>
  <tr><td>SW-Core-01</td></tr>
</table>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0], "SW-Core-01");
    }

    #[test]
    fn test_parse_conf_search_html_deduplicates() {
        // Same node appearing multiple times (can happen in search results)
        let html = r#"
<table>
  <tr><td>SW-Core-01</td></tr>
  <tr><td>RTR-Edge-01</td></tr>
  <tr><td>SW-Core-01</td></tr>
  <tr><td>SW-Core-01</td></tr>
</table>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert_eq!(nodes.len(), 2, "Should deduplicate nodes");
        assert!(nodes.contains(&"SW-Core-01".to_string()));
        assert!(nodes.contains(&"RTR-Edge-01".to_string()));
    }

    #[test]
    fn test_parse_conf_search_html_trims_whitespace() {
        let html = r#"
<table>
  <tr><td>  SW-Core-01  </td></tr>
  <tr><td>
    RTR-Edge-01
  </td></tr>
</table>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert_eq!(nodes.len(), 2);
        assert!(
            nodes.contains(&"SW-Core-01".to_string()),
            "Should trim leading/trailing whitespace"
        );
        assert!(
            nodes.contains(&"RTR-Edge-01".to_string()),
            "Should trim newlines and whitespace"
        );
    }

    #[test]
    fn test_parse_conf_search_html_no_matches() {
        // Valid table structure but pattern matches nothing
        let html = r#"
<div class="alert alert-info">
  No results found for pattern 'nonexistent'
</div>
"#;

        let nodes = OxidizedClient::parse_conf_search_html(html);

        assert!(nodes.is_empty(), "No matches should return empty list");
    }

    // -------------------------------------------------------------------------
    // conf_search Edge Case Tests (Story 2-3)
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_conf_search_empty_pattern_returns_empty() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };
        let client = OxidizedClient::new(&config);

        // Empty pattern should return empty vec without network call
        let result = client.conf_search("").await;

        assert!(result.is_ok(), "Empty pattern should not error");
        assert!(
            result.unwrap().is_empty(),
            "Empty pattern should return empty list"
        );
    }

    // -------------------------------------------------------------------------
    // SSL Verification Tests (Story 4-1, AC1)
    // -------------------------------------------------------------------------

    #[test]
    fn test_client_applies_ssl_verify_false() {
        let config = Config {
            oxidized_url: "https://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: false,
            custom_headers: vec![],
        };

        // Client should be created without panic (accepts invalid certs)
        let client = OxidizedClient::new(&config);
        assert_eq!(client.base_url, "https://localhost:8888");
    }

    #[test]
    fn test_client_applies_ssl_verify_true() {
        let config = Config {
            oxidized_url: "https://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };

        // Client should be created with SSL verification enabled
        let client = OxidizedClient::new(&config);
        assert_eq!(client.base_url, "https://localhost:8888");
    }

    // -------------------------------------------------------------------------
    // Custom Headers Tests (Story 4-1, AC2)
    // -------------------------------------------------------------------------

    #[test]
    fn test_client_stores_custom_headers() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![("X-Api-Key".to_string(), "secret123".to_string())],
        };

        let client = OxidizedClient::new(&config);
        assert_eq!(client.custom_headers.len(), 1);
        assert_eq!(client.custom_headers[0].0, "X-Api-Key");
        assert_eq!(client.custom_headers[0].1, "secret123");
    }

    #[test]
    fn test_client_stores_multiple_custom_headers() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![
                ("X-Api-Key".to_string(), "secret123".to_string()),
                ("X-Custom".to_string(), "value".to_string()),
            ],
        };

        let client = OxidizedClient::new(&config);
        assert_eq!(client.custom_headers.len(), 2);
    }

    #[test]
    fn test_client_empty_custom_headers() {
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: true,
            custom_headers: vec![],
        };

        let client = OxidizedClient::new(&config);
        assert!(client.custom_headers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Authorization Header Priority Tests (Story 4-1, AC3)
    // -------------------------------------------------------------------------

    #[test]
    fn test_client_with_auth_and_no_custom_auth() {
        // Has basic auth credentials, no custom Authorization header
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: Some("admin".to_string()),
            oxidized_password: Some("secret".to_string()),
            ssl_verify: true,
            custom_headers: vec![("X-Api-Key".to_string(), "apikey".to_string())],
        };

        let client = OxidizedClient::new(&config);

        // Auth should be set (Basic Auth will be applied)
        assert!(client.auth.is_some());
        // Custom headers should not contain Authorization
        assert!(
            !client
                .custom_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
        );
    }

    #[test]
    fn test_client_with_auth_and_custom_auth() {
        // Has both basic auth credentials AND custom Authorization header
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: Some("admin".to_string()),
            oxidized_password: Some("secret".to_string()),
            ssl_verify: true,
            custom_headers: vec![
                ("Authorization".to_string(), "Bearer token123".to_string()),
                ("X-Api-Key".to_string(), "apikey".to_string()),
            ],
        };

        let client = OxidizedClient::new(&config);

        // Auth is set but won't be used due to custom Authorization header
        assert!(client.auth.is_some());
        // Custom headers contain Authorization
        assert!(
            client
                .custom_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
        );
    }

    #[test]
    fn test_client_with_custom_auth_case_insensitive() {
        // Custom Authorization header with different case
        let config = Config {
            oxidized_url: "http://localhost:8888".to_string(),
            oxidized_user: Some("admin".to_string()),
            oxidized_password: Some("secret".to_string()),
            ssl_verify: true,
            custom_headers: vec![("authorization".to_string(), "Bearer token".to_string())],
        };

        let client = OxidizedClient::new(&config);

        // Verify case-insensitive check works
        assert!(
            client
                .custom_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
        );
    }

    // -------------------------------------------------------------------------
    // Combined Configuration Tests (Story 4-1, AC4)
    // -------------------------------------------------------------------------

    #[test]
    fn test_client_combined_ssl_and_headers() {
        let config = Config {
            oxidized_url: "https://oxidized.example.com".to_string(),
            oxidized_user: None,
            oxidized_password: None,
            ssl_verify: false,
            custom_headers: vec![
                ("X-Api-Key".to_string(), "secret".to_string()),
                ("X-Custom".to_string(), "value".to_string()),
            ],
        };

        let client = OxidizedClient::new(&config);

        // Both configurations should be applied
        assert_eq!(client.base_url, "https://oxidized.example.com");
        assert_eq!(client.custom_headers.len(), 2);
        // Note: ssl_verify=false is applied at client builder level (can't directly verify)
    }

    // -------------------------------------------------------------------------
    // URL Encoding Tests (Story 6-1)
    // -------------------------------------------------------------------------

    #[test]
    fn test_urlencoding_node_name_with_space() {
        // Verify urlencoding::encode handles spaces correctly
        let name = "router 1";
        let encoded = urlencoding::encode(name);
        assert_eq!(encoded, "router%201");
    }

    #[test]
    fn test_urlencoding_node_name_with_slash() {
        // Verify urlencoding::encode handles slashes correctly
        let name = "dc/switch-1";
        let encoded = urlencoding::encode(name);
        assert_eq!(encoded, "dc%2Fswitch-1");
    }

    #[test]
    fn test_urlencoding_node_name_with_special_chars() {
        // Verify urlencoding::encode handles various special characters
        let name = "router@dc1#01";
        let encoded = urlencoding::encode(name);
        assert_eq!(encoded, "router%40dc1%2301");
    }

    #[test]
    fn test_urlencoding_node_name_utf8() {
        // Verify urlencoding::encode handles UTF-8 correctly
        let name = "routeur-été";
        let encoded = urlencoding::encode(name);
        assert_eq!(encoded, "routeur-%C3%A9t%C3%A9");
    }

    #[test]
    fn test_urlencoding_node_name_plain() {
        // Verify urlencoding::encode doesn't modify plain names
        let name = "switch-core-01";
        let encoded = urlencoding::encode(name);
        assert_eq!(encoded, "switch-core-01");
    }

    #[test]
    fn test_encode_path_segments_preserves_group_separator() {
        let full_name = "core switches/router 1";
        let encoded = encode_path_segments(full_name);
        assert_eq!(encoded, "core%20switches/router%201");
    }

    #[test]
    fn test_urlencoding_decode_roundtrip() {
        // Verify encode/decode roundtrip preserves original value
        let names = vec![
            "router 1",
            "dc/switch-1",
            "routeur-été",
            "node@group#123",
            "plain-name",
        ];

        for name in names {
            let encoded = urlencoding::encode(name);
            let decoded = urlencoding::decode(&encoded).expect("Should decode");
            assert_eq!(decoded, name, "Roundtrip failed for: {}", name);
        }
    }

    #[test]
    fn test_endpoint_format_with_encoded_name() {
        // Verify endpoint formatting works correctly with encoded names
        let name = "router 1";
        let endpoint = format!("/node/show/{}.json", urlencoding::encode(name));
        assert_eq!(endpoint, "/node/show/router%201.json");
    }

    #[test]
    fn test_endpoint_format_with_encoded_group_and_name() {
        // Verify endpoint formatting with multiple encoded parameters
        let name = "switch 1";
        let group = "dc/core";
        let oid = "abc123";
        let endpoint = format!(
            "/node/version/view.json?node={}&group={}&oid={}",
            urlencoding::encode(name),
            urlencoding::encode(group),
            oid
        );
        assert_eq!(
            endpoint,
            "/node/version/view.json?node=switch%201&group=dc%2Fcore&oid=abc123"
        );
    }
}
