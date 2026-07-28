//! Read-only tools mirroring Oxidized MCP resources.

use regex::Regex;
use serde::Serialize;

use crate::error::OxidizedError;
use crate::oxidized::{CacheMetadata, Node, NodeVersion, OxidizedBackend, OxidizedClient};
use crate::redaction::RedactionMetadata;
use crate::resources::{
    self, ConfigMetadata, ConfigSummary, NodeResponse, PaginatedResponse, PaginationParams,
    VersionConfigResponse,
};

#[derive(Debug, Clone, Default)]
pub struct NodeFilters {
    pub group: Option<String>,
    pub name_pattern: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
}

pub async fn list_nodes(
    backend: &OxidizedClient,
    offset: usize,
    limit: usize,
    filters: NodeFilters,
) -> Result<PaginatedResponse<Node>, OxidizedError> {
    if !(1..=resources::MAX_PAGE_SIZE).contains(&limit) {
        return Err(OxidizedError::InvalidRegex(format!(
            "limit must be between 1 and {}",
            resources::MAX_PAGE_SIZE
        )));
    }
    let name_regex = filters
        .name_pattern
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|error| OxidizedError::InvalidRegex(error.to_string()))?;
    let (mut nodes, metadata) = backend.get_nodes().await?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes.retain(|node| {
        filters
            .group
            .as_ref()
            .is_none_or(|group| &node.group == group)
            && filters
                .model
                .as_ref()
                .is_none_or(|model| &node.model == model)
            && filters
                .status
                .as_ref()
                .is_none_or(|status| node.effective_status() == Some(status))
            && name_regex
                .as_ref()
                .is_none_or(|pattern| pattern.is_match(&node.name))
    });
    let pagination = PaginationParams::new(Some(offset), Some(limit));
    Ok(resources::paginate(
        nodes,
        pagination.offset,
        pagination.limit,
        metadata,
    ))
}

pub async fn get_node(backend: &OxidizedClient, node: &str) -> Result<NodeResponse, OxidizedError> {
    resources::get_node(backend, node).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMode {
    Full,
    Summary,
    Lines,
}

impl ConfigMode {
    pub fn parse(value: &str) -> Result<Self, OxidizedError> {
        match value {
            "full" => Ok(Self::Full),
            "summary" => Ok(Self::Summary),
            "lines" => Ok(Self::Lines),
            _ => Err(OxidizedError::InvalidRegex(
                "mode must be one of: full, summary, lines".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeConfigToolResponse {
    pub node: String,
    pub model: String,
    pub backup_timestamp: Option<String>,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ConfigSummary>,
    pub size: ConfigMetadata,
    pub metadata: CacheMetadata,
    pub redaction: RedactionMetadata,
}

#[allow(clippy::too_many_arguments)]
pub async fn get_node_config(
    backend: &OxidizedClient,
    node: &str,
    mode: ConfigMode,
    start_line: Option<usize>,
    end_line: Option<usize>,
    truncate_head: Option<usize>,
    truncate_tail: Option<usize>,
    force_refresh: bool,
) -> Result<NodeConfigToolResponse, OxidizedError> {
    if mode != ConfigMode::Lines && (start_line.is_some() || end_line.is_some()) {
        return Err(OxidizedError::InvalidRegex(
            "start_line/end_line require mode='lines'".to_string(),
        ));
    }
    if force_refresh {
        backend.invalidate_config(node).await;
    }
    let node_data = backend.get_node(node).await?.0;
    let response = resources::get_node_config(backend, node).await?;
    let original_size = response.size.clone();
    let (config, summary, mode_name) = match mode {
        ConfigMode::Summary => (
            None,
            Some(resources::extract_config_summary(&response.config)),
            "summary",
        ),
        ConfigMode::Lines => {
            let lines: Vec<&str> = response.config.lines().collect();
            let start = start_line.unwrap_or(1);
            let end = end_line.unwrap_or(lines.len());
            if start == 0 || end < start {
                return Err(OxidizedError::InvalidRegex(
                    "line range must be 1-based and end_line >= start_line".to_string(),
                ));
            }
            (
                Some(
                    lines
                        .into_iter()
                        .skip(start - 1)
                        .take(end - start + 1)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                None,
                "lines",
            )
        }
        ConfigMode::Full => {
            let config = if truncate_head.is_some() || truncate_tail.is_some() {
                resources::truncate_config(
                    &response.config,
                    truncate_head.unwrap_or(resources::DEFAULT_TRUNCATE_HEAD),
                    truncate_tail.unwrap_or(resources::DEFAULT_TRUNCATE_TAIL),
                )
            } else {
                response.config
            };
            (Some(config), None, "full")
        }
    };
    Ok(NodeConfigToolResponse {
        node: node.to_string(),
        model: node_data.model,
        backup_timestamp: node_data.mtime,
        mode: mode_name.to_string(),
        config,
        summary,
        size: original_size,
        metadata: response.metadata,
        redaction: response.redaction,
    })
}

pub async fn list_config_versions(
    backend: &OxidizedClient,
    node: &str,
    offset: usize,
    limit: usize,
) -> Result<PaginatedResponse<NodeVersion>, OxidizedError> {
    if !(1..=resources::MAX_PAGE_SIZE).contains(&limit) {
        return Err(OxidizedError::InvalidRegex(format!(
            "limit must be between 1 and {}",
            resources::MAX_PAGE_SIZE
        )));
    }
    let response = resources::get_node_versions(backend, node).await?;
    let pagination = PaginationParams::new(Some(offset), Some(limit));
    Ok(resources::paginate(
        response.versions,
        pagination.offset,
        pagination.limit,
        response.metadata,
    ))
}

pub async fn get_config_version(
    backend: &OxidizedClient,
    node: &str,
    oid: &str,
) -> Result<VersionConfigResponse, OxidizedError> {
    resources::get_node_version(backend, node, oid).await
}

pub async fn diff_latest(
    backend: &OxidizedClient,
    node: &str,
) -> Result<super::DiffResult, OxidizedError> {
    let versions = resources::get_node_versions(backend, node).await?;
    if versions.versions.len() < 2 {
        return Err(OxidizedError::InvalidRegex(format!(
            "Node '{node}' has fewer than two configuration versions"
        )));
    }
    super::diff_configs(
        backend,
        node,
        &versions.versions[1].oid,
        &versions.versions[0].oid,
    )
    .await
}
