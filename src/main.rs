use mcp_oxidized::config::Config;
use mcp_oxidized::error::{Actionable, OxidizedError};
use mcp_oxidized::oxidized::OxidizedClient;
use mcp_oxidized::{resources, tools};
use rmcp::model::{
    Annotated, CallToolRequestParam, CallToolResult, Content, ErrorCode, Implementation,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParam,
    ProtocolVersion, RawResource, RawResourceTemplate, ReadResourceRequestParam,
    ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
struct OxidizedServer {
    client: Arc<OxidizedClient>,
    backups: tools::BackupRegistry,
}

impl OxidizedServer {
    fn try_new(config: Config) -> Result<Self, OxidizedError> {
        Ok(Self {
            client: Arc::new(OxidizedClient::try_new(&config)?),
            backups: tools::BackupRegistry::default(),
        })
    }

    fn to_mcp_error(error: OxidizedError) -> McpError {
        let code = match error {
            OxidizedError::NodeNotFound(_, _) | OxidizedError::InvalidRegex(_) => {
                ErrorCode::INVALID_PARAMS
            }
            OxidizedError::AuthFailed | OxidizedError::ConfigError(_) => ErrorCode::INVALID_REQUEST,
            OxidizedError::ParseError { .. } => ErrorCode::PARSE_ERROR,
            OxidizedError::ApiUnreachable { .. } | OxidizedError::HttpError { .. } => {
                ErrorCode::INTERNAL_ERROR
            }
        };
        McpError::new(code, error.to_llm_message(), None)
    }
}

fn object(value: Value) -> Arc<Map<String, Value>> {
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

fn annotations(read_only: bool) -> ToolAnnotations {
    ToolAnnotations {
        title: None,
        read_only_hint: Some(read_only),
        destructive_hint: Some(false),
        idempotent_hint: Some(read_only),
        open_world_hint: Some(false),
    }
}

fn tool(
    name: &'static str,
    title: &str,
    description: &'static str,
    input_schema: Value,
    read_only: bool,
) -> Tool {
    Tool {
        name: Cow::Borrowed(name),
        title: Some(title.to_string()),
        description: Some(Cow::Borrowed(description)),
        input_schema: object(input_schema),
        output_schema: Some(object(output_schema(name))),
        annotations: Some(annotations(read_only)),
        icons: None,
        meta: None,
    }
}

fn output_schema(name: &str) -> Value {
    let cache = json!({
        "type": "object",
        "properties": {
            "cache_hit": {"type": "boolean"},
            "fresh": {"type": "boolean"}
        },
        "required": ["cache_hit", "fresh"]
    });
    let redaction = json!({
        "type": "object",
        "properties": {
            "enabled": {"type": "boolean"},
            "replacement_count": {"type": "integer", "minimum": 0}
        },
        "required": ["enabled", "replacement_count"]
    });
    let properties = match name {
        "list_nodes" | "list_config_versions" => json!({
            "items": {"type": "array"},
            "total": {"type": "integer", "minimum": 0},
            "offset": {"type": "integer", "minimum": 0},
            "limit": {"type": "integer", "minimum": 1},
            "has_more": {"type": "boolean"},
            "metadata": cache
        }),
        "get_node" => json!({"node": {"type": "object"}, "metadata": cache}),
        "get_node_config" => json!({
            "node": {"type": "string"},
            "model": {"type": "string"},
            "backup_timestamp": {"type": ["string", "null"]},
            "mode": {"type": "string", "enum": ["full", "summary", "lines"]},
            "config": {"type": "string"},
            "summary": {"type": "object"},
            "size": {"type": "object"},
            "metadata": cache,
            "redaction": redaction
        }),
        "get_config_version" => json!({
            "config": {"type": "string"},
            "oid": {"type": "string"},
            "size": {"type": "object"},
            "metadata": cache,
            "redaction": redaction
        }),
        "diff_latest" | "diff_configs" => json!({
            "node": {"type": "string"},
            "version1": {"type": "string"},
            "version2": {"type": "string"},
            "identical": {"type": "boolean"},
            "summary": {"type": "object"},
            "additions": {"type": "array"},
            "deletions": {"type": "array"},
            "modifications": {"type": "array"},
            "unified_diff": {"type": "string"},
            "redaction": redaction
        }),
        "search_configs" => json!({
            "pattern": {"type": "string"},
            "case_sensitive": {"type": "boolean"},
            "literal": {"type": "boolean"},
            "context_before": {"type": "integer"},
            "context_after": {"type": "integer"},
            "total_matches": {"type": "integer"},
            "shown_matches": {"type": "integer"},
            "offset": {"type": "integer"},
            "limit": {"type": "integer"},
            "has_more": {"type": "boolean"},
            "nodes_searched": {"type": "integer"},
            "configs_fetched": {"type": "integer"},
            "nodes_with_matches": {"type": "integer"},
            "nodes_returned": {"type": "integer"},
            "results": {"type": "array"},
            "warnings": {"type": "array", "items": {"type": "string"}},
            "redaction": redaction
        }),
        "fetch_node_config" | "get_backup_status" => json!({
            "operation_id": {"type": "string"},
            "node": {"type": "string"},
            "completion_state": {"type": "string", "enum": ["pending", "succeeded", "failed", "timed_out"]},
            "status": {"type": ["string", "null"]},
            "baseline": {"type": "object"},
            "latest": {"type": "object"},
            "mtime_changed": {"type": "boolean"},
            "completed": {"type": "boolean"},
            "message": {"type": "string"}
        }),
        "fetch_node_configs" => json!({
            "operations": {"type": "array"},
            "requested": {"type": "integer"},
            "completed": {"type": "integer"},
            "failed": {"type": "integer"},
            "pending": {"type": "integer"}
        }),
        "prioritize_node" | "reload_sources" => json!({
            "success": {"type": "boolean"},
            "message": {"type": "string"},
            "node": {"type": "string"}
        }),
        _ => json!({}),
    };
    let optional_fields: &[&str] = match name {
        "get_node_config" => &["config", "summary"],
        _ => &[],
    };
    let required = properties
        .as_object()
        .map(|object| {
            object
                .keys()
                .filter(|key| !optional_fields.contains(&key.as_str()))
                .cloned()
                .map(Value::String)
                .collect()
        })
        .unwrap_or_else(Vec::new);
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn structured<T: Serialize>(value: &T, text: String) -> Result<CallToolResult, McpError> {
    let structured_content = serde_json::to_value(value).map_err(|error| {
        McpError::new(
            ErrorCode::INTERNAL_ERROR,
            format!("Failed to serialize tool result: {error}"),
            None,
        )
    })?;
    Ok(CallToolResult {
        content: vec![Content::text(text)],
        structured_content: Some(structured_content),
        is_error: Some(false),
        meta: None,
    })
}

fn json_text<T: Serialize>(value: &T) -> Result<String, McpError> {
    serde_json::to_string_pretty(value).map_err(|error| {
        McpError::new(
            ErrorCode::INTERNAL_ERROR,
            format!("Failed to serialize response: {error}"),
            None,
        )
    })
}

fn resource<T: Serialize>(uri: String, value: &T) -> Result<ReadResourceResult, McpError> {
    Ok(ReadResourceResult {
        contents: vec![ResourceContents::TextResourceContents {
            uri,
            mime_type: Some("application/json".to_string()),
            text: json_text(value)?,
            meta: None,
        }],
    })
}

fn required_string<'a>(
    arguments: &'a Option<Map<String, Value>>,
    name: &str,
) -> Result<&'a str, McpError> {
    arguments
        .as_ref()
        .and_then(|args| args.get(name))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            McpError::new(
                ErrorCode::INVALID_PARAMS,
                format!("Missing required string parameter '{name}'"),
                None,
            )
        })
}

fn optional_usize(arguments: &Option<Map<String, Value>>, name: &str) -> Option<usize> {
    arguments
        .as_ref()
        .and_then(|args| args.get(name))
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

fn optional_bool(arguments: &Option<Map<String, Value>>, name: &str, default: bool) -> bool {
    arguments
        .as_ref()
        .and_then(|args| args.get(name))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn optional_strings(arguments: &Option<Map<String, Value>>, name: &str) -> Option<Vec<String>> {
    arguments
        .as_ref()
        .and_then(|args| args.get(name))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
}

fn validate_tool_arguments(
    tool_name: &str,
    arguments: &Option<Map<String, Value>>,
) -> Result<(), McpError> {
    let Some(arguments) = arguments.as_ref() else {
        return Ok(());
    };
    let invalid = |message: String| McpError::new(ErrorCode::INVALID_PARAMS, message, None);
    let integer = |name: &str, minimum: u64, maximum: u64| -> Result<(), McpError> {
        if let Some(value) = arguments.get(name) {
            let value = value.as_u64().ok_or_else(|| {
                invalid(format!("Parameter '{name}' must be a non-negative integer"))
            })?;
            if !(minimum..=maximum).contains(&value) {
                return Err(invalid(format!(
                    "Parameter '{name}' must be between {minimum} and {maximum}"
                )));
            }
        }
        Ok(())
    };
    let boolean = |name: &str| -> Result<(), McpError> {
        if arguments.get(name).is_some_and(|value| !value.is_boolean()) {
            return Err(invalid(format!("Parameter '{name}' must be a boolean")));
        }
        Ok(())
    };

    for name in [
        "node",
        "oid",
        "pattern",
        "group",
        "name_pattern",
        "model",
        "status",
        "version1",
        "version2",
        "operation_id",
        "mode",
    ] {
        if arguments.get(name).is_some_and(|value| !value.is_string()) {
            return Err(invalid(format!("Parameter '{name}' must be a string")));
        }
    }
    for name in ["wait", "force_refresh", "case_sensitive", "literal"] {
        boolean(name)?;
    }
    integer("offset", 0, usize::MAX as u64)?;
    integer("start_line", 1, usize::MAX as u64)?;
    integer("end_line", 1, usize::MAX as u64)?;
    integer("truncate_head", 0, usize::MAX as u64)?;
    integer("truncate_tail", 0, usize::MAX as u64)?;
    integer("timeout_seconds", 1, 300)?;
    integer("concurrency", 1, 10)?;
    integer("context_before", 0, tools::MAX_CONTEXT_LINES as u64)?;
    integer("context_after", 0, tools::MAX_CONTEXT_LINES as u64)?;
    integer("limit_per_node", 1, 1000)?;
    integer(
        "limit",
        1,
        if tool_name == "search_configs" {
            1000
        } else {
            resources::MAX_PAGE_SIZE as u64
        },
    )?;

    if let Some(nodes) = arguments.get("nodes") {
        let nodes = nodes
            .as_array()
            .ok_or_else(|| invalid("Parameter 'nodes' must be an array of strings".to_string()))?;
        if nodes.iter().any(|node| !node.is_string()) {
            return Err(invalid(
                "Parameter 'nodes' must contain only strings".to_string(),
            ));
        }
        if tool_name == "fetch_node_configs" && !(1..=20).contains(&nodes.len()) {
            return Err(invalid(
                "Parameter 'nodes' must contain between 1 and 20 nodes".to_string(),
            ));
        }
    }
    Ok(())
}

impl ServerHandler for OxidizedServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "mcp-oxidized".to_string(),
                title: Some("Oxidized MCP Server".to_string()),
                version: VERSION.to_string(),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Use list_nodes and get_node for discovery, get_node_config for masked \
                 configuration output, search_configs for deterministic cross-node search, \
                 and fetch_node_config/get_backup_status for tracked backups."
                    .to_string(),
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                Annotated::new(
                    RawResource {
                        uri: "oxidized://nodes".to_string(),
                        name: "nodes".to_string(),
                        title: Some("All Nodes".to_string()),
                        description: Some("Paginated Oxidized inventory".to_string()),
                        mime_type: Some("application/json".to_string()),
                        size: None,
                        icons: None,
                        meta: None,
                    },
                    None,
                ),
                Annotated::new(
                    RawResource {
                        uri: "oxidized://stats".to_string(),
                        name: "stats".to_string(),
                        title: Some("Statistics".to_string()),
                        description: Some("Oxidized backup statistics".to_string()),
                        mime_type: Some("application/json".to_string()),
                        size: None,
                        icons: None,
                        meta: None,
                    },
                    None,
                ),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let definitions = [
            (
                "oxidized://node/{name}",
                "node",
                "Node Details",
                "Node metadata and freshness information",
            ),
            (
                "oxidized://node/{name}/config",
                "node_config",
                "Node Configuration",
                "Latest configuration, masked by default",
            ),
            (
                "oxidized://node/{name}/versions",
                "node_versions",
                "Configuration Versions",
                "Historical configuration versions",
            ),
            (
                "oxidized://node/{name}/versions/{oid}",
                "node_version",
                "Historical Configuration",
                "Masked configuration for a specific version",
            ),
        ];
        Ok(ListResourceTemplatesResult {
            resource_templates: definitions
                .into_iter()
                .map(|(uri, name, title, description)| {
                    Annotated::new(
                        RawResourceTemplate {
                            uri_template: uri.to_string(),
                            name: name.to_string(),
                            title: Some(title.to_string()),
                            description: Some(description.to_string()),
                            mime_type: Some("application/json".to_string()),
                        },
                        None,
                    )
                })
                .collect(),
            next_cursor: None,
            meta: None,
        })
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
        let client = Arc::clone(&self.client);
        async move {
            let uri = request.uri;
            if uri == "oxidized://nodes" {
                let value = resources::list_nodes(&*client, None, None, None)
                    .await
                    .map_err(Self::to_mcp_error)?;
                return resource(uri, &value);
            }
            if uri == "oxidized://stats" {
                let value = resources::get_stats(&*client)
                    .await
                    .map_err(Self::to_mcp_error)?;
                return resource(uri, &value);
            }
            let node_uri = uri.strip_prefix("oxidized://node/").ok_or_else(|| {
                McpError::new(ErrorCode::INVALID_PARAMS, "Unknown resource URI", None)
            })?;
            let (path, query) = node_uri
                .split_once('?')
                .map_or((node_uri, None), |(path, query)| (path, Some(query)));
            if let Some(node) = path.strip_suffix("/config") {
                let node = urlencoding::decode(node).unwrap_or(Cow::Borrowed(node));
                let mut truncate = false;
                let mut truncate_head = None;
                let mut truncate_tail = None;
                let mut summary = false;
                for (key, value) in query
                    .into_iter()
                    .flat_map(|query| query.split('&'))
                    .filter_map(|pair| pair.split_once('='))
                {
                    match key {
                        "truncate" => truncate = value == "true",
                        "truncate_head" => truncate_head = value.parse().ok(),
                        "truncate_tail" => truncate_tail = value.parse().ok(),
                        "summary" => summary = value == "true",
                        _ => {}
                    }
                }
                let truncation = truncate
                    .then(|| resources::TruncationParams::new(true, truncate_head, truncate_tail));
                let value =
                    resources::get_node_config_with_options(&*client, &node, truncation, summary)
                        .await
                        .map_err(Self::to_mcp_error)?;
                return resource(uri, &value);
            }
            if let Some(node) = path.strip_suffix("/versions") {
                let node = urlencoding::decode(node).unwrap_or(Cow::Borrowed(node));
                let value = resources::get_node_versions(&*client, &node)
                    .await
                    .map_err(Self::to_mcp_error)?;
                return resource(uri, &value);
            }
            if let Some((node, oid)) = path.split_once("/versions/") {
                let node = urlencoding::decode(node).unwrap_or(Cow::Borrowed(node));
                let value = resources::get_node_version(&*client, &node, oid)
                    .await
                    .map_err(Self::to_mcp_error)?;
                return resource(uri, &value);
            }
            let node = urlencoding::decode(path).unwrap_or(Cow::Borrowed(path));
            let value = resources::get_node(&*client, &node)
                .await
                .map_err(Self::to_mcp_error)?;
            resource(uri, &value)
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let node = json!({"type":"string","minLength":1});
        let pagination = json!({
            "offset":{"type":"integer","minimum":0,"default":0},
            "limit":{"type":"integer","minimum":1,"maximum":500,"default":100}
        });
        let tools = vec![
            tool(
                "list_nodes",
                "List Nodes",
                "List and filter Oxidized nodes.",
                json!({
                    "type":"object","properties":{
                        "offset":pagination["offset"],"limit":pagination["limit"],
                        "group":{"type":"string"},"name_pattern":{"type":"string"},
                        "model":{"type":"string"},"status":{"type":"string"}
                    }
                }),
                true,
            ),
            tool(
                "get_node",
                "Get Node",
                "Get one Oxidized node.",
                json!({
                    "type":"object","properties":{"node":node},"required":["node"]
                }),
                true,
            ),
            tool(
                "get_node_config",
                "Get Node Configuration",
                "Get a masked current configuration in full, summary, or line-range mode.",
                json!({
                    "type":"object","properties":{
                        "node":node,"mode":{"type":"string","enum":["full","summary","lines"],"default":"full"},
                        "start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1},
                        "truncate_head":{"type":"integer","minimum":0},"truncate_tail":{"type":"integer","minimum":0},
                        "force_refresh":{"type":"boolean","default":false}
                    },"required":["node"]
                }),
                true,
            ),
            tool(
                "list_config_versions",
                "List Configuration Versions",
                "List historical versions newest first.",
                json!({
                    "type":"object","properties":{"node":node,"offset":pagination["offset"],"limit":pagination["limit"]},"required":["node"]
                }),
                true,
            ),
            tool(
                "get_config_version",
                "Get Configuration Version",
                "Get a masked historical configuration.",
                json!({
                    "type":"object","properties":{"node":node,"oid":{"type":"string","minLength":1}},"required":["node","oid"]
                }),
                true,
            ),
            tool(
                "diff_latest",
                "Diff Latest Configurations",
                "Compare the newest two versions.",
                json!({
                    "type":"object","properties":{"node":node},"required":["node"]
                }),
                true,
            ),
            tool(
                "diff_configs",
                "Diff Configurations",
                "Compare two version OIDs; changed secret lines remain visible but masked.",
                json!({
                    "type":"object","properties":{"node":node,"version1":{"type":"string"},"version2":{"type":"string"}},"required":["node","version1","version2"]
                }),
                true,
            ),
            tool(
                "search_configs",
                "Search Configurations",
                "Search raw configs and return masked matches with deterministic pagination.",
                json!({
                    "type":"object","properties":{
                        "pattern":{"type":"string","minLength":1},"nodes":{"type":"array","items":{"type":"string"}},
                        "case_sensitive":{"type":"boolean","default":true},"literal":{"type":"boolean","default":false},
                        "context_before":{"type":"integer","minimum":0,"maximum":50,"default":1},
                        "context_after":{"type":"integer","minimum":0,"maximum":50,"default":1},
                        "limit":{"type":"integer","minimum":1,"maximum":1000,"default":100},
                        "limit_per_node":{"type":"integer","minimum":1,"maximum":1000},
                        "offset":{"type":"integer","minimum":0,"default":0}
                    },"required":["pattern"]
                }),
                true,
            ),
            tool(
                "fetch_node_config",
                "Fetch Node Configuration",
                "Queue a tracked backup and optionally wait for completion.",
                json!({
                    "type":"object","properties":{"node":node,"wait":{"type":"boolean","default":false},"timeout_seconds":{"type":"integer","minimum":1,"maximum":300,"default":60}},"required":["node"]
                }),
                false,
            ),
            tool(
                "get_backup_status",
                "Get Backup Status",
                "Poll a tracked backup operation.",
                json!({
                    "type":"object","properties":{"operation_id":{"type":"string","minLength":1}},"required":["operation_id"]
                }),
                true,
            ),
            tool(
                "fetch_node_configs",
                "Fetch Node Configurations",
                "Queue up to 20 tracked backups.",
                json!({
                    "type":"object","properties":{
                        "nodes":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":20},
                        "wait":{"type":"boolean","default":false},
                        "timeout_seconds":{"type":"integer","minimum":1,"maximum":300,"default":60},
                        "concurrency":{"type":"integer","minimum":1,"maximum":10,"default":5}
                    },"required":["nodes"]
                }),
                false,
            ),
            tool(
                "prioritize_node",
                "Prioritize Node",
                "Move a node to the front of the Oxidized queue.",
                json!({
                    "type":"object","properties":{"node":node},"required":["node"]
                }),
                false,
            ),
            tool(
                "reload_sources",
                "Reload Sources",
                "Reload the Oxidized inventory.",
                json!({
                    "type":"object","properties":{}
                }),
                false,
            ),
        ];
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        let client = Arc::clone(&self.client);
        let backups = self.backups.clone();
        async move {
            let args = request.arguments;
            validate_tool_arguments(request.name.as_ref(), &args)?;
            match request.name.as_ref() {
                "list_nodes" => {
                    let filters = tools::NodeFilters {
                        group: args
                            .as_ref()
                            .and_then(|a| a.get("group"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        name_pattern: args
                            .as_ref()
                            .and_then(|a| a.get("name_pattern"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        model: args
                            .as_ref()
                            .and_then(|a| a.get("model"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        status: args
                            .as_ref()
                            .and_then(|a| a.get("status"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    };
                    let value = tools::list_nodes(
                        &client,
                        optional_usize(&args, "offset").unwrap_or(0),
                        optional_usize(&args, "limit").unwrap_or(100),
                        filters,
                    )
                    .await
                    .map_err(Self::to_mcp_error)?;
                    structured(
                        &value,
                        format!("Returned {} of {} nodes.", value.items.len(), value.total),
                    )
                }
                "get_node" => {
                    let value = tools::get_node(&client, required_string(&args, "node")?)
                        .await
                        .map_err(Self::to_mcp_error)?;
                    structured(
                        &value,
                        format!("Node {} ({})", value.node.name, value.node.model),
                    )
                }
                "get_node_config" => {
                    let node = required_string(&args, "node")?;
                    let mode = args
                        .as_ref()
                        .and_then(|a| a.get("mode"))
                        .and_then(Value::as_str)
                        .unwrap_or("full");
                    let value = tools::get_node_config(
                        &client,
                        node,
                        tools::ConfigMode::parse(mode).map_err(Self::to_mcp_error)?,
                        optional_usize(&args, "start_line"),
                        optional_usize(&args, "end_line"),
                        optional_usize(&args, "truncate_head"),
                        optional_usize(&args, "truncate_tail"),
                        optional_bool(&args, "force_refresh", false),
                    )
                    .await
                    .map_err(Self::to_mcp_error)?;
                    structured(
                        &value,
                        format!(
                            "Configuration for {node} ({mode}, {} lines).",
                            value.size.lines
                        ),
                    )
                }
                "list_config_versions" => {
                    let node = required_string(&args, "node")?;
                    let value = tools::list_config_versions(
                        &client,
                        node,
                        optional_usize(&args, "offset").unwrap_or(0),
                        optional_usize(&args, "limit").unwrap_or(100),
                    )
                    .await
                    .map_err(Self::to_mcp_error)?;
                    structured(
                        &value,
                        format!(
                            "Returned {} of {} versions for {node}.",
                            value.items.len(),
                            value.total
                        ),
                    )
                }
                "get_config_version" => {
                    let node = required_string(&args, "node")?;
                    let value =
                        tools::get_config_version(&client, node, required_string(&args, "oid")?)
                            .await
                            .map_err(Self::to_mcp_error)?;
                    structured(
                        &value,
                        format!("Historical configuration for {node} at {}.", value.oid),
                    )
                }
                "diff_latest" => {
                    let value = tools::diff_latest(&client, required_string(&args, "node")?)
                        .await
                        .map_err(Self::to_mcp_error)?;
                    let text = value.to_llm_format();
                    structured(&value, text)
                }
                "diff_configs" => {
                    let value = tools::diff_configs(
                        &client,
                        required_string(&args, "node")?,
                        required_string(&args, "version1")?,
                        required_string(&args, "version2")?,
                    )
                    .await
                    .map_err(Self::to_mcp_error)?;
                    let text = value.to_llm_format();
                    structured(&value, text)
                }
                "search_configs" => {
                    let pattern = required_string(&args, "pattern")?;
                    let value = tools::search_configs_with_options(
                        &client,
                        pattern,
                        tools::SearchOptions {
                            nodes: optional_strings(&args, "nodes"),
                            case_sensitive: optional_bool(&args, "case_sensitive", true),
                            literal: optional_bool(&args, "literal", false),
                            context_before: optional_usize(&args, "context_before").unwrap_or(1),
                            context_after: optional_usize(&args, "context_after").unwrap_or(1),
                            limit: optional_usize(&args, "limit").unwrap_or(100),
                            limit_per_node: optional_usize(&args, "limit_per_node"),
                            offset: optional_usize(&args, "offset").unwrap_or(0),
                        },
                    )
                    .await
                    .map_err(Self::to_mcp_error)?;
                    let text = value.to_llm_format();
                    structured(&value, text)
                }
                "fetch_node_config" => {
                    let value = backups
                        .start(
                            &client,
                            required_string(&args, "node")?,
                            optional_bool(&args, "wait", false),
                            optional_usize(&args, "timeout_seconds").unwrap_or(60) as u64,
                        )
                        .await
                        .map_err(Self::to_mcp_error)?;
                    let text = value.message.clone();
                    structured(&value, text)
                }
                "get_backup_status" => {
                    let operation_id = required_string(&args, "operation_id")?;
                    let value = backups
                        .status(&client, operation_id)
                        .await
                        .map_err(Self::to_mcp_error)?
                        .ok_or_else(|| {
                            McpError::new(
                                ErrorCode::INVALID_PARAMS,
                                format!("Unknown or expired operation ID '{operation_id}'"),
                                None,
                            )
                        })?;
                    let text = value.message.clone();
                    structured(&value, text)
                }
                "fetch_node_configs" => {
                    let nodes = optional_strings(&args, "nodes")
                        .filter(|nodes| !nodes.is_empty())
                        .ok_or_else(|| {
                            McpError::new(
                                ErrorCode::INVALID_PARAMS,
                                "Parameter 'nodes' must contain at least one node",
                                None,
                            )
                        })?;
                    let value = backups
                        .start_batch(
                            &client,
                            nodes,
                            optional_bool(&args, "wait", false),
                            optional_usize(&args, "timeout_seconds").unwrap_or(60) as u64,
                            optional_usize(&args, "concurrency").unwrap_or(5),
                        )
                        .await
                        .map_err(Self::to_mcp_error)?;
                    structured(
                        &value,
                        format!(
                            "Queued {} backup operations ({} pending, {} failed).",
                            value.requested, value.pending, value.failed
                        ),
                    )
                }
                "prioritize_node" => {
                    let value = tools::prioritize_node(&client, required_string(&args, "node")?)
                        .await
                        .map_err(Self::to_mcp_error)?;
                    structured(&value, value.message.clone())
                }
                "reload_sources" => {
                    let value = tools::reload_sources(&client)
                        .await
                        .map_err(Self::to_mcp_error)?;
                    structured(&value, value.message.clone())
                }
                name => Err(McpError::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("Unknown tool '{name}'"),
                    None,
                )),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_structured_output_schema_and_annotations() {
        let read_only = [
            "list_nodes",
            "get_node",
            "get_node_config",
            "list_config_versions",
            "get_config_version",
            "diff_latest",
            "diff_configs",
            "search_configs",
            "get_backup_status",
        ];
        let mut names = read_only.to_vec();
        names.extend([
            "fetch_node_config",
            "fetch_node_configs",
            "prioritize_node",
            "reload_sources",
        ]);

        for name in names {
            let definition = tool(
                name,
                "Test",
                "Test contract",
                json!({"type": "object", "properties": {}}),
                read_only.contains(&name),
            );
            let schema = definition.output_schema.expect("output schema");
            assert_eq!(schema.get("type"), Some(&json!("object")));
            assert!(
                schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| !properties.is_empty()),
                "{name} must declare output fields"
            );
            let annotations = definition.annotations.expect("tool annotations");
            assert_eq!(annotations.read_only_hint, Some(read_only.contains(&name)));
            assert_eq!(annotations.destructive_hint, Some(false));
        }
    }

    #[test]
    fn structured_helper_retains_text_and_typed_content() {
        let value = json!({"node": "router-1", "fresh": true});
        let result = structured(&value, "Node router-1".to_string()).expect("tool result");

        assert_eq!(result.structured_content, Some(value));
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn resource_and_tool_serialization_share_the_same_typed_value() {
        let value = json!({"node": {"name": "router-1"}, "metadata": {"fresh": true}});
        let tool_result = structured(&value, "Node router-1".to_string()).expect("tool result");
        let resource_result =
            resource("oxidized://node/router-1".to_string(), &value).expect("resource result");

        let resource_text = match &resource_result.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => text,
            _ => panic!("expected text resource"),
        };
        assert_eq!(
            serde_json::from_str::<Value>(resource_text).expect("resource JSON"),
            tool_result.structured_content.expect("structured content")
        );
    }

    #[test]
    fn tool_argument_validation_rejects_out_of_contract_values() {
        let invalid_context = Some(
            json!({"context_before": 51})
                .as_object()
                .expect("object")
                .clone(),
        );
        assert!(validate_tool_arguments("search_configs", &invalid_context).is_err());

        let invalid_nodes = Some(
            json!({"nodes": ["router-1", 42]})
                .as_object()
                .expect("object")
                .clone(),
        );
        assert!(validate_tool_arguments("fetch_node_configs", &invalid_nodes).is_err());

        let invalid_wait = Some(json!({"wait": "yes"}).as_object().expect("object").clone());
        assert!(validate_tool_arguments("fetch_node_config", &invalid_wait).is_err());
    }
}

#[tokio::main]
async fn main() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    if std::env::var("OXIDIZED_REDACT_SECRETS")
        .is_ok_and(|value| value.eq_ignore_ascii_case("false"))
    {
        warn!("OXIDIZED_REDACT_SECRETS=false: raw device secrets may be returned to MCP clients");
    }

    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => {
            error!("Configuration error: {error}");
            std::process::exit(1);
        }
    };
    let server = match OxidizedServer::try_new(config) {
        Ok(server) => server,
        Err(error) => {
            error!("Failed to initialize Oxidized client: {error}");
            std::process::exit(1);
        }
    };
    info!("mcp-oxidized v{VERSION} starting");
    let service = match server.serve(rmcp::transport::stdio()).await {
        Ok(service) => service,
        Err(error) => {
            error!("Failed to start MCP service: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = service.waiting().await {
        error!("Service error: {error}");
        std::process::exit(1);
    }
}
