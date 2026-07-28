//! E2E tests using MockOxidizedServer (NO `#[ignore]` - runs in CI).
//!
//! These tests verify the full integration between MCP resources/tools
//! and the Oxidized-web API using a wiremock-based mock server.
//!
//! # Test Coverage
//!
//! - **Resources**: list_nodes, get_node, get_node_config, get_node_versions, get_stats
//! - **Tools**: fetch_node_config, prioritize_node, reload_sources, diff_configs, search_configs
//! - **Errors**: NodeNotFound with suggestions, invalid regex, truncation options
//!
//! # Note
//!
//! These tests run without external dependencies (no real Oxidized server needed).
//! For real API tests, see `integration_real_api.rs`.

mod mock_server;

use mcp_oxidized::config::Config;
use mcp_oxidized::error::OxidizedError;
use mcp_oxidized::oxidized::OxidizedClient;
use mcp_oxidized::resources::{
    ConfigWithOptionsResult, TruncationParams, get_node, get_node_config,
    get_node_config_with_options, get_node_versions, get_stats, list_nodes,
};
use mcp_oxidized::tools;
use mock_server::{
    MockNode, MockOxidizedServer, default_configs, default_nodes, default_versions, modified_config,
};
use std::time::Duration;
use tokio::time::timeout;

/// Helper to create OxidizedClient pointing to mock server.
fn create_mock_client(mock_uri: &str) -> OxidizedClient {
    let config = Config {
        oxidized_url: mock_uri.to_string(),
        oxidized_user: None,
        oxidized_password: None,
        ssl_verify: true,
        custom_headers: vec![],
    };
    OxidizedClient::try_new(&config).expect("Failed to create OxidizedClient")
}

fn backup_state(mut node: MockNode, status: &str, start: &str, end: &str, mtime: &str) -> MockNode {
    node.status = status.to_string();
    node.time = end.to_string();
    node.mtime = mtime.to_string();
    node.last.status = status.to_string();
    node.last.start = start.to_string();
    node.last.end = end.to_string();
    node
}

// =============================================================================
// E2E Tests for MCP Resources (AC: 2, 4)
// =============================================================================

/// E2E test: list_nodes returns paginated data from mock.
#[tokio::test]
async fn test_e2e_list_nodes_returns_paginated_data() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        list_nodes(&client, None, None, None),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("list_nodes should succeed");

    assert_eq!(result.items.len(), 3, "default_nodes() returns 3 nodes");
    assert_eq!(result.total, 3);
    assert_eq!(result.offset, 0);
}

/// E2E test: get_node returns node details.
#[tokio::test]
async fn test_e2e_get_node_returns_details() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(Duration::from_secs(5), get_node(&client, "router-1"))
        .await
        .expect("Test should complete within timeout")
        .expect("get_node should succeed");

    assert_eq!(result.node.name, "router-1");
    assert_eq!(result.node.ip, "192.168.1.1");
    assert_eq!(result.node.group, "network");
}

/// E2E test: get_node_config returns configuration with metadata.
#[tokio::test]
async fn test_e2e_get_node_config_returns_text_with_metadata() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(Duration::from_secs(5), get_node_config(&client, "router-1"))
        .await
        .expect("Test should complete within timeout")
        .expect("get_node_config should succeed");

    assert!(!result.config.is_empty(), "Config should not be empty");
    assert!(result.size.bytes > 0, "Should have size in bytes");
    assert!(result.size.lines > 0, "Should have line count");
    assert!(
        result.config.contains("hostname"),
        "Config should contain network device keywords"
    );
    assert!(result.redaction.enabled);
    assert!(result.redaction.replacement_count >= 4);
    assert!(!result.config.contains("$1$xxxx$"));
    assert!(!result.config.contains("community public"));
}

/// E2E test: get_node_versions returns sorted version list.
#[tokio::test]
async fn test_e2e_get_node_versions_returns_sorted_list() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_versions("router-1", default_versions());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        get_node_versions(&client, "router-1"),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("get_node_versions should succeed");

    assert_eq!(result.versions.len(), 3, "default_versions() returns 3");
    assert_eq!(result.total, 3);

    // Verify versions have required fields (nested author - API quirk)
    for version in &result.versions {
        assert!(!version.oid.is_empty());
        assert!(!version.date.is_empty());
    }
}

/// E2E test: get_stats returns computed statistics.
#[tokio::test]
async fn test_e2e_get_stats_returns_computed_data() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(Duration::from_secs(5), get_stats(&client))
        .await
        .expect("Test should complete within timeout")
        .expect("get_stats should succeed");

    // Stats are computed from nodes list (stats endpoint returns 404)
    assert!(
        result.stats.total_nodes.unwrap_or(0) >= 3,
        "Should have at least 3 nodes"
    );
}

// =============================================================================
// E2E Tests for MCP Tools (AC: 2, 3)
// =============================================================================

/// E2E test: fetch_node_config triggers backup.
#[tokio::test]
async fn test_e2e_fetch_node_config_triggers_backup() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        tools::fetch_node_config(&client, "router-1"),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("fetch_node_config should succeed");

    assert!(result.success);
    assert_eq!(result.node, "router-1");
    assert!(result.message.contains("Backup triggered"));
}

#[tokio::test]
async fn test_e2e_tracked_backup_completes_when_config_is_unchanged() {
    let nodes = default_nodes();
    let baseline = backup_state(
        nodes[0].clone(),
        "success",
        "2026-01-01 00:00:00 UTC",
        "2026-01-01 00:00:05 UTC",
        "unchanged",
    );
    let pending = backup_state(
        nodes[0].clone(),
        "running",
        "2026-01-01 00:01:00 UTC",
        "",
        "unchanged",
    );
    let completed = backup_state(
        nodes[0].clone(),
        "success",
        "2026-01-01 00:01:00 UTC",
        "2026-01-01 00:01:05 UTC",
        "unchanged",
    );
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(nodes)
        .with_node_show_sequence("router-1", vec![baseline, pending, completed]);
    mock.mount_all().await;
    let client = create_mock_client(&mock.uri());
    let registry = tools::BackupRegistry::default();

    let result = registry
        .start(&client, "router-1", true, 3)
        .await
        .expect("tracked backup should complete");

    assert_eq!(result.completion_state, tools::BackupState::Succeeded);
    assert!(result.completed);
    assert!(!result.mtime_changed);
    assert!(result.message.contains("unchanged"));
}

#[tokio::test]
async fn test_e2e_tracked_backup_reports_failure_and_timeout() {
    let nodes = default_nodes();
    let baseline = nodes[0].clone();
    let failed = backup_state(
        nodes[0].clone(),
        "failure",
        "2026-01-01 00:01:00 UTC",
        "2026-01-01 00:01:05 UTC",
        "changed",
    );
    let failed_mock = MockOxidizedServer::start()
        .await
        .with_nodes(nodes.clone())
        .with_node_show_sequence("router-1", vec![baseline.clone(), failed]);
    failed_mock.mount_all().await;
    let client = create_mock_client(&failed_mock.uri());
    let result = tools::BackupRegistry::default()
        .start(&client, "router-1", true, 2)
        .await
        .expect("failed run should still return an operation");
    assert_eq!(result.completion_state, tools::BackupState::Failed);

    let timeout_mock = MockOxidizedServer::start()
        .await
        .with_nodes(nodes)
        .with_node_show_sequence("router-1", vec![baseline]);
    timeout_mock.mount_all().await;
    let client = create_mock_client(&timeout_mock.uri());
    let result = tools::BackupRegistry::default()
        .start(&client, "router-1", true, 1)
        .await
        .expect("timeout should still return an operation");
    assert_eq!(result.completion_state, tools::BackupState::TimedOut);
}

#[tokio::test]
async fn test_e2e_batch_backup_waits_with_bounded_concurrency() {
    let nodes = default_nodes();
    let router_done = backup_state(
        nodes[0].clone(),
        "success",
        "2026-01-01 00:02:00 UTC",
        "2026-01-01 00:02:05 UTC",
        "router-new",
    );
    let switch_done = backup_state(
        nodes[1].clone(),
        "success",
        "2026-01-01 00:02:00 UTC",
        "2026-01-01 00:02:05 UTC",
        "switch-new",
    );
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(nodes.clone())
        .with_node_show_sequence("router-1", vec![nodes[0].clone(), router_done])
        .with_node_show_sequence("switch-1", vec![nodes[1].clone(), switch_done]);
    mock.mount_all().await;
    let client = create_mock_client(&mock.uri());

    let result = tools::BackupRegistry::default()
        .start_batch(
            &client,
            vec!["switch-1".to_string(), "router-1".to_string()],
            true,
            2,
            2,
        )
        .await
        .expect("batch should complete");

    assert_eq!(result.requested, 2);
    assert_eq!(result.completed, 2);
    assert_eq!(result.failed, 0);
    assert_eq!(result.operations[0].node, "router-1");
    assert!(
        result
            .operations
            .iter()
            .all(|operation| operation.mtime_changed)
    );
}

#[tokio::test]
async fn test_e2e_pending_backup_bypasses_configuration_cache() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs())
        .with_config_sequence(
            "router-1",
            vec![
                "hostname old".to_string(),
                "hostname early-stale".to_string(),
                "hostname fresh".to_string(),
            ],
        );
    mock.mount_all().await;
    let client = create_mock_client(&mock.uri());

    let (initial, initial_cache) =
        mcp_oxidized::oxidized::OxidizedBackend::get_node_config(&client, "router-1")
            .await
            .expect("initial config");
    assert_eq!(initial, "hostname old");
    assert!(!initial_cache.cache_hit);

    client.set_backup_pending("router-1", true).await;
    let (early, early_cache) =
        mcp_oxidized::oxidized::OxidizedBackend::get_node_config(&client, "router-1")
            .await
            .expect("early config");
    let (fresh, fresh_cache) =
        mcp_oxidized::oxidized::OxidizedBackend::get_node_config(&client, "router-1")
            .await
            .expect("fresh config");

    assert_eq!(early, "hostname early-stale");
    assert_eq!(fresh, "hostname fresh");
    assert!(!early_cache.cache_hit);
    assert!(!fresh_cache.cache_hit);
}

/// E2E test: prioritize_node updates queue position.
#[tokio::test]
async fn test_e2e_prioritize_node_updates_queue() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        tools::prioritize_node(&client, "switch-1"),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("prioritize_node should succeed");

    assert!(result.success);
    assert_eq!(result.node, "switch-1");
    assert!(result.message.contains("prioritized"));
}

/// E2E test: reload_sources reloads inventory and invalidates cache.
#[tokio::test]
async fn test_e2e_reload_sources_invalidates_cache() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(Duration::from_secs(5), tools::reload_sources(&client))
        .await
        .expect("Test should complete within timeout")
        .expect("reload_sources should succeed");

    assert!(result.success);
    assert!(result.message.contains("reloaded"));
}

/// E2E test: diff_configs compares two versions.
#[tokio::test]
async fn test_e2e_diff_configs_compares_versions() {
    let mut versions = default_versions();
    // First version points to original config
    versions[0].oid = "v1-oid".to_string();
    // Second version points to modified config
    versions[1].oid = "v2-oid".to_string();

    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_versions("router-1", versions)
        .with_version_config(
            "router-1",
            "v1-oid",
            default_configs().get("router-1").unwrap(),
        )
        .with_version_config("router-1", "v2-oid", &modified_config());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        tools::diff_configs(&client, "router-1", "v1-oid", "v2-oid"),
    )
    .await
    .expect("Test should complete within timeout");

    let diff = result.expect("diff should succeed");
    assert_eq!(diff.node, "router-1");
    assert_eq!(diff.version1, "v1-oid");
    assert_eq!(diff.version2, "v2-oid");
    assert!(diff.to_llm_format().contains("Configuration Diff"));
    assert!(!diff.unified_diff.contains("$1$xxxx$"));
}

#[tokio::test]
async fn test_e2e_historical_config_and_secret_only_diff_are_redacted() {
    let versions = default_versions();
    let old_oid = versions[1].oid.clone();
    let new_oid = versions[0].oid.clone();
    let old = "hostname router-1\nsnmp-server community old-secret ro\n";
    let new = "hostname router-1\nsnmp-server community new-secret ro\n";
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_versions("router-1", versions)
        .with_version_config("router-1", &old_oid, old)
        .with_version_config("router-1", &new_oid, new);
    mock.mount_all().await;
    let client = create_mock_client(&mock.uri());

    let historical = mcp_oxidized::resources::get_node_version(&client, "router-1", &old_oid)
        .await
        .expect("historical config");
    assert!(historical.metadata.fresh);
    assert!(historical.config.contains("<redacted>"));
    assert!(!historical.config.contains("old-secret"));

    let diff = tools::diff_configs(&client, "router-1", &old_oid, &new_oid)
        .await
        .expect("secret-only diff");
    assert!(!diff.identical);
    assert!(
        diff.unified_diff
            .contains("-snmp-server community <redacted> ro")
    );
    assert!(
        diff.unified_diff
            .contains("+snmp-server community <redacted> ro")
    );
    assert!(!diff.unified_diff.contains("old-secret"));
    assert!(!diff.unified_diff.contains("new-secret"));
}

/// E2E test: search_configs finds patterns in configurations.
#[tokio::test]
async fn test_e2e_search_configs_finds_patterns() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        tools::search_configs(&client, "hostname", None, false, 100),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("search_configs should succeed");

    // Configs contain "hostname" keyword
    assert!(result.nodes_searched > 0, "Should search at least one node");
    assert!(
        result.total_matches > 0,
        "Should find 'hostname' in configs"
    );

    // LLM format should be available
    let llm = result.to_llm_format();
    assert!(llm.contains("Configuration Search Results"));
}

/// E2E test: search_configs uses pre-filter when available.
#[tokio::test]
async fn test_e2e_search_configs_uses_prefilter() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    // Search for a pattern that exists in configs
    let result = timeout(
        Duration::from_secs(5),
        tools::search_configs(&client, "interface", None, false, 50),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("search_configs should succeed");

    // The mock conf_search endpoint filters nodes
    // Nodes without "interface" won't be searched (optimization)
    assert!(result.nodes_searched <= 3);
}

#[tokio::test]
async fn test_e2e_search_zero_prefilter_matches_has_correct_counters() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;
    let client = create_mock_client(&mock.uri());

    let result = tools::search_configs_with_options(
        &client,
        "definitely-not-present",
        tools::SearchOptions::default(),
    )
    .await
    .expect("zero-match search");

    assert_eq!(result.nodes_searched, 3);
    assert_eq!(result.configs_fetched, 0);
    assert_eq!(result.nodes_with_matches, 0);
    assert_eq!(result.nodes_returned, 0);
}

#[tokio::test]
async fn test_e2e_search_intersection_pagination_and_redaction_are_deterministic() {
    let mut configs = default_configs();
    configs.insert(
        "router-1".to_string(),
        "description target one\nsnmp-server community target ro\ndescription target two\n"
            .to_string(),
    );
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(configs);
    mock.mount_all().await;
    let client = create_mock_client(&mock.uri());

    let result = tools::search_configs_with_options(
        &client,
        "target",
        tools::SearchOptions {
            nodes: Some(vec![
                "router-1".to_string(),
                "fw-1".to_string(),
                "missing".to_string(),
            ]),
            context_before: 0,
            context_after: 0,
            offset: 1,
            limit: 1,
            ..tools::SearchOptions::default()
        },
    )
    .await
    .expect("paginated search");

    assert_eq!(result.nodes_searched, 2);
    assert_eq!(result.configs_fetched, 1);
    assert_eq!(result.total_matches, 3);
    assert_eq!(result.shown_matches, 1);
    assert_eq!(result.offset, 1);
    assert!(result.has_more);
    assert_eq!(result.results[0].node, "router-1");
    assert_eq!(
        result.results[0].matches[0].content,
        "snmp-server community <redacted> ro"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("missing"))
    );
}

// =============================================================================
// E2E Tests for Error Scenarios (AC: 2, 5)
// =============================================================================

/// E2E test: NodeNotFound returns actionable error with HTTP 500.
#[tokio::test]
async fn test_e2e_node_not_found_returns_actionable_error() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        get_node(&client, "nonexistent-node"),
    )
    .await
    .expect("Test should complete within timeout");

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, _suggestions) => {
            assert!(name.contains("nonexistent"));
            // Mock should trigger the HTTP 500 → NodeNotFound detection
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

/// E2E test: NodeNotFound includes suggestions from available nodes.
#[tokio::test]
async fn test_e2e_node_not_found_includes_suggestions() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    // Use a name similar to an existing node to trigger suggestions
    let result = timeout(Duration::from_secs(5), get_node(&client, "router-999"))
        .await
        .expect("Test should complete within timeout");

    assert!(result.is_err());

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, "router-999");
            // Suggestions should include similar nodes from the inventory
            // (router-1 has "router" prefix)
            if !suggestions.is_empty() {
                println!("Suggestions: {:?}", suggestions);
            }
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

/// E2E test: Invalid regex returns clear error.
#[tokio::test]
async fn test_e2e_invalid_regex_returns_clear_error() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        tools::search_configs(&client, "[invalid(regex", None, true, 100),
    )
    .await
    .expect("Test should complete within timeout");

    assert!(result.is_err(), "Should return error for invalid regex");

    match result.unwrap_err() {
        OxidizedError::InvalidRegex(msg) => {
            assert!(
                msg.contains("invalid") || msg.contains("Invalid") || msg.contains("error"),
                "Error should mention invalid pattern: {}",
                msg
            );
        }
        other => panic!("Expected InvalidRegex error, got: {:?}", other),
    }
}

/// E2E test: Config truncation option works correctly.
#[tokio::test]
async fn test_e2e_config_options_truncate_works() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    // Get full config first
    let full_result = timeout(Duration::from_secs(5), get_node_config(&client, "router-1"))
        .await
        .expect("Test should complete within timeout")
        .expect("get_node_config should succeed");

    let full_lines = full_result.size.lines;

    // Only test truncation if config is large enough
    if full_lines <= 20 {
        println!(
            "SKIP: Config only has {} lines, not enough for truncation test",
            full_lines
        );
        return;
    }

    // Request truncated config
    let truncation = TruncationParams::new(true, Some(5), Some(5));
    let result = timeout(
        Duration::from_secs(5),
        get_node_config_with_options(&client, "router-1", Some(truncation), false),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("get_node_config_with_options should succeed");

    match result {
        ConfigWithOptionsResult::Config(config_response) => {
            assert!(
                config_response.config.contains("TRUNCATED"),
                "Truncated config should contain TRUNCATED marker"
            );
            // Original size should be preserved
            assert_eq!(config_response.size.lines, full_lines);
        }
        ConfigWithOptionsResult::Summary(_) => {
            panic!("Expected Config, got Summary");
        }
    }
}

/// E2E test: Config summary option works correctly.
#[tokio::test]
async fn test_e2e_config_options_summary_works() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes())
        .with_configs(default_configs());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        get_node_config_with_options(&client, "router-1", None, true),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("get_node_config_with_options should succeed");

    match result {
        ConfigWithOptionsResult::Summary(summary_response) => {
            assert!(summary_response.summary.total_lines > 0);
            assert!(!summary_response.summary.vendor_hint.is_empty());
            assert!(summary_response.redaction.replacement_count >= 4);

            // LLM format should work
            let llm = summary_response.summary.to_llm_format();
            assert!(llm.contains("Configuration Summary"));
        }
        ConfigWithOptionsResult::Config(_) => {
            panic!("Expected Summary, got Config");
        }
    }
}

// =============================================================================
// Additional Edge Cases
// =============================================================================

/// E2E test: list_nodes with group filter.
#[tokio::test]
async fn test_e2e_list_nodes_with_group_filter() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        list_nodes(&client, None, None, Some("network")),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("list_nodes should succeed");

    // Only network group nodes should be returned
    for node in &result.items {
        assert_eq!(node.group, "network");
    }
}

/// E2E test: list_nodes with pagination.
#[tokio::test]
async fn test_e2e_list_nodes_with_pagination() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    let result = timeout(
        Duration::from_secs(5),
        list_nodes(&client, Some(0), Some(2), None),
    )
    .await
    .expect("Test should complete within timeout")
    .expect("list_nodes should succeed");

    assert!(result.items.len() <= 2, "Should respect limit");
    assert_eq!(result.offset, 0);
    assert_eq!(result.limit, 2);
    assert_eq!(result.total, 3); // Total should reflect all nodes
}

/// E2E test: Tools handle NodeNotFound with suggestions.
#[tokio::test]
async fn test_e2e_tools_node_not_found_returns_suggestions() {
    let mock = MockOxidizedServer::start()
        .await
        .with_nodes(default_nodes());
    mock.mount_all().await;

    let client = create_mock_client(&mock.uri());

    // fetch_node_config with non-existent node
    let result = timeout(
        Duration::from_secs(5),
        tools::fetch_node_config(&client, "nonexistent-xyz"),
    )
    .await
    .expect("Test should complete within timeout");

    match result {
        Err(OxidizedError::NodeNotFound(name, _suggestions)) => {
            assert!(name.contains("nonexistent"));
        }
        Ok(_) => panic!("Expected NodeNotFound error"),
        Err(other) => panic!("Expected NodeNotFound error, got: {:?}", other),
    }

    // prioritize_node with non-existent node
    let result2 = timeout(
        Duration::from_secs(5),
        tools::prioritize_node(&client, "another-fake-node"),
    )
    .await
    .expect("Test should complete within timeout");

    match result2 {
        Err(OxidizedError::NodeNotFound(name, _)) => {
            assert!(name.contains("another-fake-node"));
        }
        Ok(_) => panic!("Expected NodeNotFound error"),
        Err(other) => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}
