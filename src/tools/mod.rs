//! MCP Tools for Oxidized node operations.
//!
//! Tools include both write operations that modify Oxidized state and
//! read operations that provide enhanced analysis:
//!
//! **Write Operations:**
//! - [`fetch_node_config`] - Trigger immediate backup (FR15)
//! - [`prioritize_node`] - Prioritize node in queue (FR16)
//! - [`reload_sources`] - Reload source inventory (FR17)
//!
//! **Analysis Operations:**
//! - [`diff_configs`] - Compare two configuration versions (FR9)
//! - [`search_configs`] - Search for patterns across configurations (FR10-FR13)
//!
//! # Cache Invalidation Rule
//!
//! All tools that modify state MUST invalidate relevant caches on success.
//! The backend methods handle this automatically, so tools do not need to
//! call invalidate explicitly.
//!
//! | Tool | Action | Cache Invalidation |
//! |------|--------|-------------------|
//! | `fetch_node_config(node)` | Trigger backup | `invalidate_node(node)` (via backend) |
//! | `prioritize_node(node)` | Prioritize in queue | `invalidate_node(node)` (via backend) |
//! | `reload_sources()` | Reload inventory | `invalidate_all_nodes()` (via backend) |
//!
//! # Example
//!
//! ```ignore
//! use mcp_oxidized::tools::{fetch_node_config, prioritize_node, reload_sources};
//! use mcp_oxidized::oxidized::OxidizedClient;
//!
//! let client = OxidizedClient::new(&config);
//!
//! // Trigger immediate backup for a node
//! let result = fetch_node_config(&client, "SW-Core-01").await?;
//! println!("{}", result.message);
//!
//! // Prioritize a node in the backup queue
//! let result = prioritize_node(&client, "SW-Core-01").await?;
//! println!("{}", result.message);
//!
//! // Reload the source inventory
//! let result = reload_sources(&client).await?;
//! println!("{}", result.message);
//! ```

mod backup;
mod diff_configs;
mod fetch_node_config;
mod prioritize_node;
mod read;
mod reload_sources;
mod search_configs;

pub use backup::{BackupMetadata, BackupOperation, BackupRegistry, BackupState, BatchBackupResult};
pub use diff_configs::{DiffResult, diff_configs};
pub use fetch_node_config::fetch_node_config;
pub use prioritize_node::prioritize_node;
pub use read::{
    ConfigMode, NodeConfigToolResponse, NodeFilters, diff_latest, get_config_version, get_node,
    get_node_config, list_config_versions, list_nodes,
};
pub use reload_sources::reload_sources;
pub use search_configs::{
    MAX_CONTEXT_LINES, NodeMatches, SearchMatch, SearchOptions, SearchResult, search_configs,
    search_configs_with_options,
};

use crate::error::OxidizedError;
use crate::oxidized::{OxidizedBackend, OxidizedClient};
use crate::resources::{MAX_SUGGESTIONS, find_similar_nodes};
use serde::Serialize;

/// Result of a tool operation.
///
/// Provides structured feedback about tool execution including success status,
/// human-readable message, and the node that was operated on.
///
/// # Example
///
/// ```
/// use mcp_oxidized::tools::ToolResult;
///
/// let result = ToolResult::success("SW-Core-01", "Backup triggered successfully");
/// assert!(result.success);
/// assert_eq!(result.node, "SW-Core-01");
/// ```
#[derive(Debug, Clone, Serialize)]
#[must_use = "ToolResult should be returned to the caller"]
pub struct ToolResult {
    /// Whether the operation succeeded.
    pub success: bool,
    /// Human-readable message describing the result.
    pub message: String,
    /// The node that was operated on.
    pub node: String,
}

impl ToolResult {
    /// Create a success result.
    ///
    /// # Arguments
    ///
    /// * `node` - The node name that was operated on
    /// * `message` - A human-readable success message
    ///
    /// # Example
    ///
    /// ```
    /// use mcp_oxidized::tools::ToolResult;
    ///
    /// let result = ToolResult::success("SW-01", "Backup triggered");
    /// assert!(result.success);
    /// ```
    pub fn success(node: &str, message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            node: node.to_string(),
        }
    }

    /// Create a failure result.
    ///
    /// # Arguments
    ///
    /// * `node` - The node name that was operated on
    /// * `message` - A human-readable failure message
    ///
    /// # Example
    ///
    /// ```
    /// use mcp_oxidized::tools::ToolResult;
    ///
    /// let result = ToolResult::failure("SW-01", "Node not found");
    /// assert!(!result.success);
    /// ```
    pub fn failure(node: &str, message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            node: node.to_string(),
        }
    }
}

/// Enrich a NodeNotFound error with similar node suggestions.
///
/// Fetches the node list from the backend and finds nodes with similar names
/// to provide helpful suggestions to the user.
///
/// # Arguments
///
/// * `backend` - The Oxidized client to fetch nodes from
/// * `node_name` - The node name that was not found
///
/// # Returns
///
/// An `OxidizedError::NodeNotFound` with populated suggestions.
pub(crate) async fn enrich_node_not_found(
    backend: &OxidizedClient,
    node_name: String,
) -> OxidizedError {
    let suggestions = match backend.get_nodes().await {
        Ok((nodes, _)) => find_similar_nodes(&nodes, &node_name, MAX_SUGGESTIONS),
        Err(_) => vec![],
    };
    OxidizedError::NodeNotFound(node_name, suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("SW-01", "Backup triggered");
        assert!(result.success);
        assert_eq!(result.node, "SW-01");
        assert!(result.message.contains("Backup"));
    }

    #[test]
    fn test_tool_result_failure() {
        let result = ToolResult::failure("SW-01", "Node not found");
        assert!(!result.success);
        assert_eq!(result.node, "SW-01");
    }

    #[test]
    fn test_tool_result_serializes() {
        let result = ToolResult::success("SW-01", "Test message");
        let json = serde_json::to_string(&result).expect("Should serialize");
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"node\":\"SW-01\""));
        assert!(json.contains("\"message\":"));
    }

    #[test]
    fn test_tool_result_success_with_into_string() {
        let result = ToolResult::success("node", String::from("Message"));
        assert!(result.success);
        assert_eq!(result.message, "Message");
    }

    #[test]
    fn test_tool_result_failure_with_into_string() {
        let result = ToolResult::failure("node", String::from("Error"));
        assert!(!result.success);
        assert_eq!(result.message, "Error");
    }

    #[test]
    fn test_reload_sources_result_message() {
        let result = ToolResult::success(
            "",
            "Oxidized sources reloaded. New devices are now available in the inventory.",
        );
        assert!(result.success);
        assert!(result.message.contains("reloaded"));
        assert!(result.node.is_empty());
    }
}
