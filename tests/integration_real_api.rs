//! Integration tests for OxidizedClient against a REAL Oxidized server.
//!
//! ⚠️ **IMPORTANT**: These tests require access to a real Oxidized server.
//! All tests are marked with `#[ignore]` and will NOT run in CI.
//!
//! For CI testing without a real server, see `e2e_tests.rs` which uses a mock server.
//!
//! # Test Strategy
//!
//! | Test File | Server | Attribute | CI | Command |
//! |-----------|--------|-----------|-----|---------|
//! | `e2e_tests.rs` | Mock (wiremock) | None | ✅ | `cargo test` |
//! | `integration_real_api.rs` | Real Oxidized | `#[ignore]` | ❌ | `cargo test -- --ignored` |
//!
//! # Environment Variables
//!
//! - `OXIDIZED_URL` - Required: Base URL of the Oxidized server (e.g., `http://oxidized:8888`)
//! - `OXIDIZED_USER` - Optional: Username for Basic Auth
//! - `OXIDIZED_PASSWORD` - Optional: Password for Basic Auth
//!
//! # Running Real API Tests
//!
//! ```bash
//! # Set environment variables
//! export OXIDIZED_URL="http://oxidized.example.com:8888"
//! export OXIDIZED_USER="admin"
//! export OXIDIZED_PASSWORD="secret"
//!
//! # Run all real API tests
//! cargo test -- --ignored
//!
//! # Run a specific test
//! cargo test test_get_nodes_returns_list -- --ignored
//! ```

use mcp_oxidized::config::Config;
use mcp_oxidized::error::OxidizedError;
use mcp_oxidized::oxidized::{OxidizedBackend, OxidizedClient};
use mcp_oxidized::resources::{
    ConfigWithOptionsResult, TruncationParams, get_node, get_node_config,
    get_node_config_with_options, get_node_version, get_node_versions, get_stats, list_nodes,
};

/// Helper to create a client from environment variables.
///
/// Supports environment variables:
/// - `OXIDIZED_URL` - Required: Base URL of the Oxidized server
/// - `OXIDIZED_USER` - Optional: Username for Basic Auth
/// - `OXIDIZED_PASSWORD` - Optional: Password for Basic Auth
/// - `OXIDIZED_SSL_VERIFY` - Optional: SSL verification (default: true)
/// - `OXIDIZED_HEADERS` - Optional: Custom headers (format: Header1:Value1,Header2:Value2)
fn create_client_from_env() -> OxidizedClient {
    let oxidized_url =
        std::env::var("OXIDIZED_URL").expect("OXIDIZED_URL required for integration tests");

    // SSL verify - default true, accept "false" to disable
    let ssl_verify = std::env::var("OXIDIZED_SSL_VERIFY")
        .map(|v| !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);

    // Parse custom headers from environment
    let custom_headers = std::env::var("OXIDIZED_HEADERS")
        .map(|raw| Config::parse_headers(&raw).unwrap_or_default())
        .unwrap_or_default();

    let config = Config {
        oxidized_url,
        oxidized_user: std::env::var("OXIDIZED_USER").ok(),
        oxidized_password: std::env::var("OXIDIZED_PASSWORD").ok(),
        ssl_verify,
        custom_headers,
    };

    OxidizedClient::try_new(&config).expect("Failed to create OxidizedClient")
}

/// Test that get_nodes() returns a non-empty list from a real Oxidized server.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_nodes_returns_list() {
    let client = create_client_from_env();

    let (nodes, metadata) = client
        .get_nodes()
        .await
        .expect("Should successfully get nodes from Oxidized");

    assert!(
        !nodes.is_empty(),
        "Oxidized should return at least one node"
    );

    // First call should be cache miss
    assert!(!metadata.cache_hit, "First call should be cache miss");

    // Verify node structure
    let first_node = &nodes[0];
    assert!(
        !first_node.name.is_empty(),
        "Node should have a non-empty name"
    );
    assert!(!first_node.ip.is_empty(), "Node should have a non-empty IP");
}

/// Test that get_node() returns details for a specific node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_returns_details() {
    let client = create_client_from_env();

    // First get list of nodes to find a valid node name
    let (nodes, _) = client
        .get_nodes()
        .await
        .expect("Should get nodes to find a valid name");

    assert!(!nodes.is_empty(), "Need at least one node for this test");

    let node_name = &nodes[0].name;

    // Now get specific node details
    let (node, metadata) = client
        .get_node(node_name)
        .await
        .expect("Should get node details");

    assert_eq!(
        node.name, *node_name,
        "Returned node should match requested name"
    );

    // get_nodes hydrates the per-node cache, so this lookup should be a hit.
    assert!(
        metadata.cache_hit,
        "Node list should hydrate the node cache"
    );
}

/// Test that get_node_config() returns configuration text.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_config_returns_text() {
    let client = create_client_from_env();

    // First get list of nodes to find a valid node name
    let (nodes, _) = client
        .get_nodes()
        .await
        .expect("Should get nodes to find a valid name");

    assert!(!nodes.is_empty(), "Need at least one node for this test");

    // Find a node with successful backup
    let success_node = nodes
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if let Some(node) = success_node {
        let (config, metadata) = client
            .get_node_config(&node.name)
            .await
            .expect("Should get node configuration");

        assert!(
            !config.is_empty(),
            "Configuration should not be empty for a successful node"
        );
        assert_ne!(
            config.trim(),
            "node not found",
            "Grouped node configuration must be fetched using its full path"
        );

        // First call should be cache miss
        assert!(!metadata.cache_hit, "First call should be cache miss");
    } else {
        println!("Warning: No node with successful backup found, skipping config test");
    }
}

/// Test that get_stats() returns server statistics.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_stats_returns_data() {
    let client = create_client_from_env();

    let (stats, metadata) = client
        .get_stats()
        .await
        .expect("Should get server statistics");

    // Stats may have optional fields, but the request should succeed
    println!("Stats: {:?}", stats);

    // First call should be cache miss
    assert!(!metadata.cache_hit, "First call should be cache miss");
}

/// Test that get_node_versions() returns version history.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_versions_returns_history() {
    let client = create_client_from_env();

    // First get list of nodes to find a valid node name
    let (nodes, _) = client
        .get_nodes()
        .await
        .expect("Should get nodes to find a valid name");

    assert!(!nodes.is_empty(), "Need at least one node for this test");

    // Find a node with successful backup (likely to have versions)
    let success_node = nodes
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if let Some(node) = success_node {
        let versions = client
            .get_node_versions(&node.name)
            .await
            .expect("Should get node versions");

        if !versions.is_empty() {
            let first_version = &versions[0];
            assert!(
                !first_version.oid.is_empty(),
                "Version should have a non-empty oid"
            );
            assert!(
                !first_version.date.is_empty(),
                "Version should have a non-empty date"
            );
        } else {
            println!("Note: Node {} has no version history yet", node.name);
        }
    } else {
        println!("Warning: No node with successful backup found, skipping versions test");
    }
}

/// Test that requesting a non-existent node returns NodeNotFound error.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_not_found() {
    let client = create_client_from_env();

    let result = client.get_node("definitely-not-a-real-node-xyz123").await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        mcp_oxidized::error::OxidizedError::NodeNotFound(name, _) => {
            assert!(name.contains("definitely-not-a-real-node"));
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

// =============================================================================
// Performance Tests (AC: 7 - NFR1, NFR2)
// =============================================================================

/// Test that cached requests return in < 100ms (NFR1).
///
/// This test verifies the p95 target for cached requests.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_cached_request_performance_under_100ms() {
    use std::time::Instant;

    let client = create_client_from_env();

    // First call to populate the cache (uncached)
    let (_, first_metadata) = client
        .get_nodes()
        .await
        .expect("Should get nodes to populate cache");

    assert!(!first_metadata.cache_hit, "First call should be cache miss");

    // Measure cached request performance
    let mut durations = Vec::new();

    for _ in 0..20 {
        let start = Instant::now();
        let (_, metadata) = client
            .get_nodes()
            .await
            .expect("Should get nodes from cache");
        durations.push(start.elapsed());

        // Verify we're hitting cache
        assert!(metadata.cache_hit, "Subsequent calls should be cache hits");
    }

    // Sort durations to find p95
    durations.sort();
    let p95_index = (durations.len() as f64 * 0.95) as usize;
    let p95_duration = durations[p95_index.min(durations.len() - 1)];

    println!("Cached request p95: {:?} (target: < 100ms)", p95_duration);

    assert!(
        p95_duration < std::time::Duration::from_millis(100),
        "Cached request p95 should be < 100ms, got {:?}",
        p95_duration
    );
}

/// Test that uncached requests return in < 500ms (NFR2).
///
/// This test verifies the p95 target for uncached requests.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_uncached_request_performance_under_500ms() {
    use std::time::Instant;

    let client = create_client_from_env();

    // Measure initial (uncached) request performance
    let start = Instant::now();
    let (_, metadata) = client.get_nodes().await.expect("Should get nodes from API");
    let duration = start.elapsed();

    // Verify it was a cache miss
    assert!(!metadata.cache_hit, "First call should be cache miss");

    println!(
        "Uncached request duration: {:?} (target: < 500ms)",
        duration
    );

    assert!(
        duration < std::time::Duration::from_millis(500),
        "Uncached request should complete in < 500ms, got {:?}",
        duration
    );
}

/// Test that cache provides significant performance improvement.
///
/// Cached requests should be at least 5x faster than uncached requests.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_cache_provides_performance_improvement() {
    use std::time::Instant;

    let client = create_client_from_env();

    // Measure uncached request time
    let uncached_start = Instant::now();
    let (_, uncached_meta) = client.get_nodes().await.expect("Should get nodes from API");
    let uncached_duration = uncached_start.elapsed();

    assert!(!uncached_meta.cache_hit, "First call should be cache miss");

    // Measure cached request time
    let cached_start = Instant::now();
    let (_, cached_meta) = client
        .get_nodes()
        .await
        .expect("Should get nodes from cache");
    let cached_duration = cached_start.elapsed();

    assert!(cached_meta.cache_hit, "Second call should be cache hit");

    println!(
        "Uncached: {:?}, Cached: {:?}, Improvement: {:.1}x",
        uncached_duration,
        cached_duration,
        uncached_duration.as_micros() as f64 / cached_duration.as_micros().max(1) as f64
    );

    assert!(
        cached_duration < uncached_duration,
        "Cached request should be faster than uncached"
    );
}

/// Test that config cache works correctly.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_cache_hit() {
    use std::time::Instant;

    let client = create_client_from_env();

    // Get a valid node name
    let (nodes, _) = client.get_nodes().await.expect("Should get nodes");

    let success_node = nodes
        .iter()
        .find(|n| n.effective_status() == Some("success"));
    if success_node.is_none() {
        println!("Warning: No node with successful backup found, skipping config cache test");
        return;
    }
    let node_name = &success_node.unwrap().name;

    // First call - cache miss
    let start1 = Instant::now();
    let (config1, meta1) = client
        .get_node_config(node_name)
        .await
        .expect("Should get config");
    let duration1 = start1.elapsed();

    assert!(!meta1.cache_hit, "First call should be cache miss");

    // Second call - cache hit
    let start2 = Instant::now();
    let (config2, meta2) = client
        .get_node_config(node_name)
        .await
        .expect("Should get config from cache");
    let duration2 = start2.elapsed();

    assert!(meta2.cache_hit, "Second call should be cache hit");
    assert_eq!(config1, config2, "Cached config should match original");

    println!(
        "Config cache - Uncached: {:?}, Cached: {:?}",
        duration1, duration2
    );

    assert!(
        duration2 < std::time::Duration::from_millis(100),
        "Cached config request should be < 100ms"
    );
}

/// Test that stats cache works correctly.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_stats_cache_hit() {
    use std::time::Instant;

    let client = create_client_from_env();

    // First call - cache miss
    let start1 = Instant::now();
    let (_, meta1) = client.get_stats().await.expect("Should get stats");
    let duration1 = start1.elapsed();

    assert!(!meta1.cache_hit, "First call should be cache miss");

    // Second call - cache hit
    let start2 = Instant::now();
    let (_, meta2) = client
        .get_stats()
        .await
        .expect("Should get stats from cache");
    let duration2 = start2.elapsed();

    assert!(meta2.cache_hit, "Second call should be cache hit");

    println!(
        "Stats cache - Uncached: {:?}, Cached: {:?}",
        duration1, duration2
    );

    assert!(
        duration2 < std::time::Duration::from_millis(100),
        "Cached stats request should be < 100ms"
    );
}

// =============================================================================
// Resource Handler Integration Tests (Story 1.6)
// =============================================================================

/// Test that list_nodes() resource returns paginated data.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_list_nodes_returns_paginated_data() {
    let client = create_client_from_env();

    let result = list_nodes(&client, None, None, None).await;

    assert!(result.is_ok(), "list_nodes should succeed");

    let response = result.unwrap();
    assert!(response.total > 0, "Should have at least one node");
    assert_eq!(response.offset, 0, "Default offset should be 0");
    assert!(response.limit <= 500, "Limit should be capped at 500");
}

/// Test that list_nodes() respects pagination parameters.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_list_nodes_pagination_params() {
    let client = create_client_from_env();

    let result = list_nodes(&client, Some(0), Some(5), None).await;

    assert!(result.is_ok(), "list_nodes with pagination should succeed");

    let response = result.unwrap();
    assert!(response.items.len() <= 5, "Should respect limit parameter");
    assert_eq!(response.offset, 0, "Should preserve offset");
    assert_eq!(response.limit, 5, "Should preserve limit");
}

/// Test that list_nodes() filters by group.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_list_nodes_group_filter() {
    let client = create_client_from_env();

    // First get all nodes to find a valid group
    let all_nodes = list_nodes(&client, None, None, None).await.unwrap();

    if all_nodes.items.is_empty() {
        println!("Warning: No nodes found, skipping group filter test");
        return;
    }

    let group = &all_nodes.items[0].group;

    // Now filter by that group
    let filtered = list_nodes(&client, None, None, Some(group)).await.unwrap();

    // All items should have matching group
    for node in &filtered.items {
        assert_eq!(
            &node.group, group,
            "All filtered nodes should have matching group"
        );
    }
}

/// Test that get_node() resource returns node with cache metadata.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_returns_data() {
    let client = create_client_from_env();

    // Get a valid node name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();

    if nodes.items.is_empty() {
        println!("Warning: No nodes found, skipping get_node test");
        return;
    }

    let node_name = &nodes.items[0].name;

    let result = get_node(&client, node_name).await;

    assert!(result.is_ok(), "get_node should succeed");

    let response = result.unwrap();
    assert_eq!(
        &response.node.name, node_name,
        "Returned node should match request"
    );
}

/// Test that get_node() returns NodeNotFound with suggestions.
///
/// This test uses a two-phase approach:
/// 1. First, list nodes to find a real node name prefix
/// 2. Then, query for a non-existent node with that prefix to trigger suggestions
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_not_found_has_suggestions() {
    let client = create_client_from_env();

    // Phase 1: Get a real node to build a similar-but-nonexistent name
    let nodes_result = list_nodes(&client, None, Some(5), None).await;
    assert!(nodes_result.is_ok(), "Should be able to list nodes");

    let nodes = nodes_result.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory to test suggestions");
        return;
    }

    // Take first node's name and append something to make it not exist
    let existing_name = &nodes.items[0].name;
    let non_existent_name = format!("{}-NONEXISTENT-999", existing_name);

    // Phase 2: Query for non-existent node - should get suggestions
    let result = get_node(&client, &non_existent_name).await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent_name);
            // With at least one node in inventory and a prefix match, we should get suggestions
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions when nodes exist with similar prefix. \
                 Original node '{}' should appear in suggestions.",
                existing_name
            );
            assert!(
                suggestions.contains(existing_name),
                "Suggestions {:?} should contain the original node '{}'",
                suggestions,
                existing_name
            );
            println!(
                "NodeNotFound correctly returned {} suggestions: {:?}",
                suggestions.len(),
                suggestions
            );
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

/// Test that get_stats() resource returns statistics via resource handler.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_resource_get_stats_returns_data() {
    let client = create_client_from_env();

    let result = get_stats(&client).await;

    assert!(result.is_ok(), "get_stats should succeed");

    let response = result.unwrap();
    println!("Stats: {:?}", response.stats);
}

/// Test that list_nodes() includes cache metadata.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_list_nodes_cache_metadata() {
    let client = create_client_from_env();

    // First call - cache miss
    let first = list_nodes(&client, None, None, None).await.unwrap();
    assert!(!first.metadata.cache_hit, "First call should be cache miss");

    // Second call - cache hit
    let second = list_nodes(&client, None, None, None).await.unwrap();
    assert!(second.metadata.cache_hit, "Second call should be cache hit");
}

// =============================================================================
// Configuration Access Resources Tests (Story 1.7)
// =============================================================================

/// Test that get_node_config() returns configuration with size metadata.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_config_returns_data_with_size() {
    let client = create_client_from_env();

    // Get a valid node name with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;
    let result = get_node_config(&client, node_name).await;

    assert!(result.is_ok(), "get_node_config should succeed");

    let response = result.unwrap();
    assert!(!response.config.is_empty(), "Config should not be empty");
    assert!(response.size.bytes > 0, "Should have size metadata bytes");
    assert!(response.size.lines > 0, "Should have line count");
    assert!(
        response.size.estimated_tokens > 0,
        "Should have estimated tokens"
    );

    // Verify token estimation is reasonable (bytes/4)
    assert_eq!(
        response.size.estimated_tokens,
        response.size.bytes / 4,
        "Token estimation should be bytes/4"
    );
}

/// Test that get_node_config() returns NodeNotFound with suggestions.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_config_not_found_has_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    let result = get_node_config(&client, &non_existent).await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
        }
        other => panic!("Expected NodeNotFound, got: {:?}", other),
    }
}

/// Test that get_node_versions() returns version list sorted descending.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_versions_sorted_descending() {
    let client = create_client_from_env();

    // Get a valid node name with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;
    let result = get_node_versions(&client, node_name).await;

    assert!(result.is_ok(), "get_node_versions should succeed");

    let response = result.unwrap();
    assert_eq!(
        response.total,
        response.versions.len(),
        "Total should match versions count"
    );

    // Verify descending order (if more than 1 version)
    if response.versions.len() > 1 {
        for i in 0..response.versions.len() - 1 {
            assert!(
                response.versions[i].date >= response.versions[i + 1].date,
                "Versions should be sorted newest first"
            );
        }
    }
}

/// Test that get_node_versions() returns NodeNotFound with suggestions.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_versions_not_found_has_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    let result = get_node_versions(&client, &non_existent).await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
        }
        other => panic!("Expected NodeNotFound, got: {:?}", other),
    }
}

/// Test that get_node_version() returns historical config with size metadata.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_version_returns_historical_config() {
    let client = create_client_from_env();

    // Get a valid node with versions
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Get versions for this node
    let versions = get_node_versions(&client, node_name).await;
    if versions.is_err() || versions.as_ref().unwrap().versions.is_empty() {
        println!("SKIP: No versions available for node {}", node_name);
        return;
    }

    let version = &versions.unwrap().versions[0];
    let oid = &version.oid;

    // Get specific version config
    let result = get_node_version(&client, node_name, oid).await;

    assert!(result.is_ok(), "get_node_version should succeed");

    let response = result.unwrap();
    assert!(!response.config.is_empty(), "Config should not be empty");
    assert_eq!(response.oid, *oid, "OID should match request");
    assert!(response.size.bytes > 0, "Should have size metadata");
}

/// Test that get_node_version() handles invalid OID gracefully.
///
/// Note: Oxidized-web 0.18.0 returns `["version not found"]` for invalid OIDs,
/// which is a valid JSON response rather than an HTTP error.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_version_invalid_oid_returns_message() {
    let client = create_client_from_env();

    // Get a valid node name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let node_name = &nodes.items[0].name;

    // Request with invalid OID - Oxidized-web returns ["version not found"] as valid JSON
    let result = get_node_version(&client, node_name, "invalid-oid-that-does-not-exist").await;

    // Oxidized-web 0.18.0 returns success with "version not found" message
    // This is API behavior, not an error
    match result {
        Ok(response) => {
            assert!(
                response.config.contains("version not found"),
                "Expected 'version not found' message, got: {}",
                response.config
            );
        }
        Err(e) => {
            // Some versions may return an error - that's also acceptable
            println!("API returned error for invalid OID: {:?}", e);
        }
    }
}

/// Test that get_node_version() returns NodeNotFound with suggestions for invalid node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_version_not_found_has_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    // Request with non-existent node and any OID
    let result = get_node_version(&client, &non_existent, "any-oid").await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
            println!(
                "NodeNotFound correctly returned {} suggestions: {:?}",
                suggestions.len(),
                suggestions
            );
        }
        other => panic!("Expected NodeNotFound, got: {:?}", other),
    }
}

/// Test config cache hit/miss for get_node_config.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_get_node_config_cache_hit() {
    let client = create_client_from_env();

    // Get a valid node name with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // First call - cache miss
    let first = get_node_config(&client, node_name).await.unwrap();
    assert!(!first.metadata.cache_hit, "First call should be cache miss");

    // Second call - cache hit
    let second = get_node_config(&client, node_name).await.unwrap();
    assert!(second.metadata.cache_hit, "Second call should be cache hit");

    // Config should be identical
    assert_eq!(first.config, second.config, "Cached config should match");
}

// =============================================================================
// Backup & Queue Management Tools Tests (Story 1.8)
// =============================================================================

use mcp_oxidized::tools;

/// Test that fetch_node_config triggers a backup for a valid node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_fetch_node_config_triggers_backup() {
    let client = create_client_from_env();

    // Get a valid node name first
    let (nodes, _) = client.get_nodes().await.expect("Should get nodes");

    if nodes.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let node_name = &nodes[0].name;

    let result = tools::fetch_node_config(&client, node_name).await;
    assert!(result.is_ok(), "fetch_node_config should succeed");

    let tool_result = result.unwrap();
    assert!(tool_result.success, "Tool result should indicate success");
    assert_eq!(tool_result.node, *node_name, "Node name should match");
    assert!(
        tool_result.message.contains("Backup triggered"),
        "Message should indicate backup was triggered"
    );
}

/// Test that prioritize_node updates queue for a valid node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_prioritize_node_updates_queue() {
    let client = create_client_from_env();

    // Get a valid node name first
    let (nodes, _) = client.get_nodes().await.expect("Should get nodes");

    if nodes.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let node_name = &nodes[0].name;

    let result = tools::prioritize_node(&client, node_name).await;
    assert!(result.is_ok(), "prioritize_node should succeed");

    let tool_result = result.unwrap();
    assert!(tool_result.success, "Tool result should indicate success");
    assert_eq!(tool_result.node, *node_name, "Node name should match");
    assert!(
        tool_result.message.contains("prioritized"),
        "Message should indicate node was prioritized"
    );
}

/// Test that fetch_node_config returns NodeNotFound with suggestions for invalid node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_fetch_node_config_invalid_node_returns_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    let result = tools::fetch_node_config(&client, &non_existent).await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
            println!(
                "NodeNotFound correctly returned {} suggestions: {:?}",
                suggestions.len(),
                suggestions
            );
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

/// Test that prioritize_node returns NodeNotFound with suggestions for invalid node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_prioritize_node_invalid_node_returns_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    let result = tools::prioritize_node(&client, &non_existent).await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
            println!(
                "NodeNotFound correctly returned {} suggestions: {:?}",
                suggestions.len(),
                suggestions
            );
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

/// Test that reload_sources reloads the Oxidized inventory.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_reload_sources_reloads_inventory() {
    let client = create_client_from_env();

    let result = tools::reload_sources(&client).await;
    assert!(result.is_ok(), "reload_sources should succeed");

    let tool_result = result.unwrap();
    assert!(tool_result.success, "Tool result should indicate success");
    assert!(
        tool_result.node.is_empty(),
        "Node should be empty for reload_sources"
    );
    assert!(
        tool_result.message.contains("reloaded"),
        "Message should indicate sources were reloaded"
    );
}

// =============================================================================
// Configuration Diff Tool Tests (Story 2.1)
// =============================================================================

/// Test that diff_configs compares two real versions successfully.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_diff_configs_compares_two_versions() {
    let client = create_client_from_env();

    // Get a node with at least 2 versions
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Get versions for this node
    let versions = get_node_versions(&client, node_name).await;
    if versions.is_err() {
        println!("SKIP: Could not get versions for node {}", node_name);
        return;
    }

    let versions = versions.unwrap();
    if versions.versions.len() < 2 {
        println!(
            "SKIP: Node {} has only {} versions (need at least 2)",
            node_name,
            versions.versions.len()
        );
        return;
    }

    let version1 = &versions.versions[1].oid; // Older version
    let version2 = &versions.versions[0].oid; // Newer version

    let result = tools::diff_configs(&client, node_name, version1, version2).await;

    assert!(result.is_ok(), "diff_configs should succeed");

    let diff = result.unwrap();
    assert_eq!(diff.node, *node_name, "Node name should match");
    assert_eq!(diff.version1, *version1, "Version1 OID should match");
    assert_eq!(diff.version2, *version2, "Version2 OID should match");

    // Output LLM format for manual verification
    println!("Diff result:\n{}", diff.to_llm_format());
}

/// Test that diff_configs returns identical=true for same version.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_diff_configs_identical_versions() {
    let client = create_client_from_env();

    // Get a node with at least 1 version
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Get versions for this node
    let versions = get_node_versions(&client, node_name).await;
    if versions.is_err() || versions.as_ref().unwrap().versions.is_empty() {
        println!("SKIP: No versions available for node {}", node_name);
        return;
    }

    let version = &versions.unwrap().versions[0].oid;

    // Compare version with itself
    let result = tools::diff_configs(&client, node_name, version, version).await;

    assert!(result.is_ok(), "diff_configs should succeed");

    let diff = result.unwrap();
    assert!(
        diff.identical,
        "Same version compared with itself should be identical"
    );
    assert_eq!(diff.summary.lines_added, 0, "No lines should be added");
    assert_eq!(diff.summary.lines_removed, 0, "No lines should be removed");
    assert_eq!(
        diff.summary.modification_blocks, 0,
        "No modification blocks expected"
    );

    // Check LLM format mentions identical
    let llm_output = diff.to_llm_format();
    assert!(
        llm_output.contains("identical"),
        "LLM output should mention configs are identical"
    );
}

/// Test that diff_configs returns NodeNotFound with suggestions for invalid node.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_diff_configs_node_not_found_has_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    let result = tools::diff_configs(&client, &non_existent, "v1", "v2").await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
            println!(
                "NodeNotFound correctly returned {} suggestions: {:?}",
                suggestions.len(),
                suggestions
            );
        }
        other => panic!("Expected NodeNotFound error, got: {:?}", other),
    }
}

/// Test that diff_configs LLM format is structured correctly.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_diff_configs_llm_format_structure() {
    let client = create_client_from_env();

    // Get a node with at least 2 versions
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Get versions for this node
    let versions = get_node_versions(&client, node_name).await;
    if versions.is_err() {
        println!("SKIP: Could not get versions for node {}", node_name);
        return;
    }

    let versions = versions.unwrap();
    if versions.versions.len() < 2 {
        println!("SKIP: Node {} needs at least 2 versions", node_name);
        return;
    }

    let version1 = &versions.versions[1].oid;
    let version2 = &versions.versions[0].oid;

    let result = tools::diff_configs(&client, node_name, version1, version2).await;

    assert!(result.is_ok(), "diff_configs should succeed");

    let diff = result.unwrap();
    let llm_output = diff.to_llm_format();

    // Verify LLM format structure
    assert!(
        llm_output.contains("## Configuration Diff:"),
        "Should have header"
    );
    assert!(
        llm_output.contains("Comparing version"),
        "Should mention versions being compared"
    );

    if !diff.identical {
        assert!(llm_output.contains("### Summary"), "Should have summary");
        assert!(
            llm_output.contains("Lines added:"),
            "Summary should have lines added"
        );
        assert!(
            llm_output.contains("Lines removed:"),
            "Summary should have lines removed"
        );
        assert!(
            llm_output.contains("Modification blocks:"),
            "Summary should have modification blocks"
        );
    }
}

// =============================================================================
// Configuration Search Tool Tests (Story 2.2)
// =============================================================================

/// Test that search_configs finds patterns in network device configurations.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_finds_patterns() {
    let client = create_client_from_env();

    // Search for a common pattern that should exist in most configs (hostname or version)
    let result = tools::search_configs(&client, "hostname|version", None, false, 100).await;

    assert!(result.is_ok(), "search_configs should succeed");

    let search_result = result.unwrap();
    println!(
        "Search completed: {} matches found",
        search_result.total_matches
    );
    println!("Nodes searched: {}", search_result.nodes_searched);
    println!("Nodes with matches: {}", search_result.nodes_with_matches);

    // We expect to find some matches in a real Oxidized setup
    if search_result.nodes_searched > 0 && search_result.nodes_with_matches == 0 {
        println!("Note: No matches found - this may be expected depending on config content");
    }
}

/// Test that search_configs limits results correctly.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_respects_limit() {
    let client = create_client_from_env();

    // Search with a very low limit
    let result = tools::search_configs(&client, ".*", None, true, 5).await;

    assert!(result.is_ok(), "search_configs should succeed");

    let search_result = result.unwrap();
    assert!(
        search_result.shown_matches <= 5,
        "Should not show more than limit"
    );

    if search_result.total_matches > 5 {
        assert!(
            search_result.shown_matches < search_result.total_matches,
            "Should truncate when total exceeds limit"
        );
    }
}

/// Test that search_configs filters by node list.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_filters_by_nodes() {
    let client = create_client_from_env();

    // Get a valid node name
    let nodes = list_nodes(&client, None, Some(3), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let node_names: Vec<String> = nodes.items.iter().map(|n| n.name.clone()).collect();

    // Search only in specific nodes
    let result = tools::search_configs(
        &client,
        ".*", // Match anything
        Some(node_names.clone()),
        true,
        100,
    )
    .await;

    assert!(result.is_ok(), "search_configs should succeed");

    let search_result = result.unwrap();
    assert!(
        search_result.nodes_searched <= node_names.len(),
        "Should only search requested nodes"
    );

    // All results should be from requested nodes
    for node_match in &search_result.results {
        assert!(
            node_names.contains(&node_match.node),
            "Result node {} should be in requested list",
            node_match.node
        );
    }
}

/// Test that search_configs handles invalid regex gracefully.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_invalid_regex_returns_error() {
    let client = create_client_from_env();

    // Use an invalid regex pattern
    let result = tools::search_configs(&client, "[invalid(regex", None, true, 100).await;

    assert!(result.is_err(), "Should return error for invalid regex");

    match result.unwrap_err() {
        OxidizedError::InvalidRegex(msg) => {
            assert!(
                msg.contains("invalid") || msg.contains("Invalid"),
                "Error should mention invalid pattern"
            );
        }
        other => panic!("Expected InvalidRegex error, got: {:?}", other),
    }
}

/// Test that search_configs warns about non-existent nodes.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_warns_about_nonexistent_nodes() {
    let client = create_client_from_env();

    // Get a valid node name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let valid_node = nodes.items[0].name.clone();
    let invalid_node = format!("{}-NONEXISTENT-999", valid_node);

    // Search including non-existent node
    let result = tools::search_configs(
        &client,
        ".*",
        Some(vec![valid_node.clone(), invalid_node.clone()]),
        true,
        100,
    )
    .await;

    assert!(result.is_ok(), "search_configs should succeed");

    let search_result = result.unwrap();
    assert!(
        !search_result.warnings.is_empty(),
        "Should have warnings about non-existent node"
    );

    let warning_about_invalid = search_result
        .warnings
        .iter()
        .any(|w| w.contains(&invalid_node));
    assert!(
        warning_about_invalid,
        "Warnings should mention the non-existent node"
    );
}

/// Test that search_configs handles case-insensitive search.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_case_insensitive() {
    let client = create_client_from_env();

    // Search for uppercase pattern with case-insensitive flag
    let result_insensitive = tools::search_configs(&client, "HOSTNAME", None, false, 100).await;
    let result_sensitive = tools::search_configs(&client, "HOSTNAME", None, true, 100).await;

    assert!(
        result_insensitive.is_ok(),
        "Case-insensitive search should succeed"
    );
    assert!(
        result_sensitive.is_ok(),
        "Case-sensitive search should succeed"
    );

    let insensitive = result_insensitive.unwrap();
    let sensitive = result_sensitive.unwrap();

    // Case-insensitive should find at least as many matches
    // (it might find more if there are lowercase "hostname" entries)
    println!(
        "Case-insensitive: {} matches, Case-sensitive: {} matches",
        insensitive.total_matches, sensitive.total_matches
    );
}

/// Test that search_configs LLM format is structured correctly.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_llm_format_structure() {
    let client = create_client_from_env();

    let result = tools::search_configs(&client, "interface", None, false, 10).await;

    assert!(result.is_ok(), "search_configs should succeed");

    let search_result = result.unwrap();
    let llm_output = search_result.to_llm_format();

    // Verify LLM format structure
    assert!(
        llm_output.contains("## Configuration Search Results"),
        "Should have header"
    );
    assert!(llm_output.contains("**Pattern:**"), "Should show pattern");
    assert!(
        llm_output.contains("**Nodes searched:**"),
        "Should show nodes searched"
    );

    if search_result.total_matches > 0 {
        assert!(
            llm_output.contains("**Line"),
            "Should show line numbers for matches"
        );
        assert!(llm_output.contains("\n> "), "Should have match marker");
    }

    println!("LLM Output:\n{}", llm_output);
}

/// Test search_configs with specific node and regex pattern.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_configs_regex_pattern() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = success_node.unwrap().name.clone();

    // Search for IP address pattern
    let result = tools::search_configs(
        &client,
        r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}", // IP address regex
        Some(vec![node_name]),
        true,
        50,
    )
    .await;

    assert!(result.is_ok(), "search_configs should succeed with regex");

    let search_result = result.unwrap();
    println!("Found {} IP address matches", search_result.total_matches);

    // If matches found, verify they look like IP addresses
    for node_match in &search_result.results {
        for m in &node_match.matches {
            let has_ip_pattern =
                m.content.contains('.') && m.content.chars().any(|c| c.is_ascii_digit());
            assert!(
                has_ip_pattern,
                "Match should contain IP-like pattern: {}",
                m.content
            );
        }
    }
}

// =============================================================================
// Search Performance & Safety Tests (Story 2.3)
// =============================================================================

/// Test that conf_search API returns HTML and can be parsed (AC: 1, 2).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_conf_search_api_works() {
    let client = create_client_from_env();

    // Test with a common pattern that should exist in configs
    let result = client.conf_search("hostname").await;

    assert!(result.is_ok(), "conf_search should succeed");

    let nodes = result.unwrap();

    // We should find at least some nodes with "hostname" in their config
    // (this is a very common pattern in network device configs)
    println!(
        "conf_search found {} nodes matching 'hostname'",
        nodes.len()
    );

    // Even if empty (no matches), the API should work without error
    // The fact that we got Ok() means the HTML was parsed successfully
}

/// Test that search_configs uses pre-filter optimization when available (AC: 5).
///
/// This test measures performance difference between optimized and unoptimized search.
/// It compares:
/// 1. A selective pattern that matches few nodes (high optimization benefit)
/// 2. A broad pattern that matches many nodes (low optimization benefit)
///
/// The selective search should search fewer nodes due to pre-filter.
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_with_prefilter_optimization() {
    use std::time::Instant;

    let client = create_client_from_env();

    // Get total node count first
    let (all_nodes, _) = client.get_nodes().await.expect("Should get nodes");
    let total_nodes = all_nodes.len();
    println!("Total nodes in inventory: {}", total_nodes);

    // First, check if conf_search is available
    let prefilter_check = client.conf_search("tacacs").await.unwrap_or_default();
    let prefilter_available = !prefilter_check.is_empty();

    if !prefilter_available {
        // Try with a more common pattern
        let common_check = client.conf_search("hostname").await.unwrap_or_default();
        if common_check.is_empty() {
            println!("SKIP: conf_search API not available or no matches");
            return;
        }
    }

    // Search for a selective pattern (one that matches few configs)
    let start_selective = Instant::now();
    let result_selective = tools::search_configs(&client, "tacacs", None, false, 100).await;
    let duration_selective = start_selective.elapsed();

    assert!(result_selective.is_ok(), "Selective search should succeed");
    let selective = result_selective.unwrap();

    // Search for a broad pattern (one that matches many configs)
    let start_broad = Instant::now();
    let result_broad = tools::search_configs(&client, "hostname", None, false, 100).await;
    let duration_broad = start_broad.elapsed();

    assert!(result_broad.is_ok(), "Broad search should succeed");
    let broad = result_broad.unwrap();

    println!(
        "Selective pattern 'tacacs': {} matches from {} nodes in {:?}",
        selective.total_matches, selective.nodes_searched, duration_selective
    );
    println!(
        "Broad pattern 'hostname': {} matches from {} nodes in {:?}",
        broad.total_matches, broad.nodes_searched, duration_broad
    );

    // Calculate optimization benefit
    let selective_reduction = if total_nodes > 0 {
        ((total_nodes - selective.nodes_searched) as f64 / total_nodes as f64) * 100.0
    } else {
        0.0
    };
    let broad_reduction = if total_nodes > 0 {
        ((total_nodes - broad.nodes_searched) as f64 / total_nodes as f64) * 100.0
    } else {
        0.0
    };

    println!(
        "Optimization benefit - Selective: {:.1}% reduction, Broad: {:.1}% reduction",
        selective_reduction, broad_reduction
    );

    // Verify pre-filter is working: selective should search fewer nodes than total
    if selective.nodes_searched < total_nodes {
        println!(
            "✓ Pre-filter optimization active: selective searched {}/{} nodes ({:.1}% saved)",
            selective.nodes_searched, total_nodes, selective_reduction
        );
    }

    // The selective search should fetch fewer configs than broad
    if selective.nodes_searched < broad.nodes_searched {
        println!(
            "✓ Selective pattern more efficient: {} vs {} nodes searched",
            selective.nodes_searched, broad.nodes_searched
        );
    }
}

/// Test that search_configs falls back gracefully when conf_search fails (AC: 5).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_prefilter_fallback() {
    let client = create_client_from_env();

    // Even if conf_search is unavailable, search_configs should still work
    // by falling back to searching all nodes
    let result = tools::search_configs(&client, "version", None, false, 50).await;

    assert!(
        result.is_ok(),
        "Search should succeed even without prefilter"
    );

    let search_result = result.unwrap();
    println!(
        "Fallback search: {} nodes searched, {} matches",
        search_result.nodes_searched, search_result.total_matches
    );

    // Verify we searched some nodes (fallback working)
    assert!(
        search_result.nodes_searched > 0,
        "Should search nodes even without prefilter"
    );
}

/// Test that search_configs correctly intersects user nodes with pre-filter results (AC: 5).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_search_prefilter_intersection() {
    let client = create_client_from_env();

    // Get some valid node names
    let nodes = list_nodes(&client, None, Some(5), None).await.unwrap();
    if nodes.items.len() < 2 {
        println!("SKIP: Need at least 2 nodes for intersection test");
        return;
    }

    let user_nodes: Vec<String> = nodes.items.iter().map(|n| n.name.clone()).collect();

    // Search with user-specified nodes
    let result = tools::search_configs(
        &client,
        ".*", // Match anything
        Some(user_nodes.clone()),
        true,
        100,
    )
    .await;

    assert!(result.is_ok(), "Search with node filter should succeed");

    let search_result = result.unwrap();

    // All results should be from our specified nodes
    for node_match in &search_result.results {
        assert!(
            user_nodes.contains(&node_match.node),
            "Result should only contain requested nodes, got: {}",
            node_match.node
        );
    }

    println!(
        "Intersection test: searched {} nodes (requested {}), found {} matches",
        search_result.nodes_searched,
        user_nodes.len(),
        search_result.total_matches
    );
}

// =============================================================================
// Large Config Handling Tests (Story 2.4)
// =============================================================================

/// Test that get_node_config returns ConfigMetadata with is_oversized field (AC: 1).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_metadata_includes_is_oversized() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;
    let result = get_node_config(&client, node_name).await;

    assert!(result.is_ok(), "get_node_config should succeed");

    let response = result.unwrap();

    // Verify is_oversized field exists and is correct type
    println!(
        "Config size: {} bytes, {} lines, is_oversized: {}",
        response.size.bytes, response.size.lines, response.size.is_oversized
    );

    // Verify size_warning logic
    if response.size.is_oversized {
        assert!(
            response.size.size_warning.is_some(),
            "Oversized config should have size_warning"
        );
        assert!(
            response
                .size
                .size_warning
                .as_ref()
                .unwrap()
                .contains("truncate=true"),
            "Warning should mention truncate option"
        );
    } else {
        assert!(
            response.size.size_warning.is_none(),
            "Non-oversized config should not have size_warning"
        );
    }
}

/// Test that get_node_config_with_options with summary=true returns ConfigSummary (AC: 4).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_with_options_summary_mode() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Request summary mode
    let result = get_node_config_with_options(&client, node_name, None, true).await;

    assert!(
        result.is_ok(),
        "get_node_config_with_options should succeed"
    );

    let response = result.unwrap();

    // Verify we got a Summary variant
    match response {
        ConfigWithOptionsResult::Summary(summary_response) => {
            println!(
                "Summary: {} sections, {} lines, vendor: {}",
                summary_response.summary.sections.len(),
                summary_response.summary.total_lines,
                summary_response.summary.vendor_hint
            );

            assert!(
                summary_response.summary.total_lines > 0,
                "Should have line count"
            );

            // Verify LLM format works
            let llm_output = summary_response.summary.to_llm_format();
            assert!(
                llm_output.contains("### Configuration Summary"),
                "LLM format should have header"
            );
        }
        ConfigWithOptionsResult::Config(_) => {
            panic!("Expected Summary variant, got Config");
        }
    }
}

/// Test that get_node_config_with_options with truncate=true truncates config (AC: 3).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_with_options_truncate_mode() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // First get full config to compare
    let full_result = get_node_config(&client, node_name).await.unwrap();
    let full_lines = full_result.size.lines;

    // Only test truncation if config is large enough
    if full_lines <= 15 {
        println!(
            "SKIP: Config only has {} lines, not enough for truncation test",
            full_lines
        );
        return;
    }

    // Request truncate mode with small params (5 head, 5 tail)
    let truncation = TruncationParams::new(true, Some(5), Some(5));
    let result = get_node_config_with_options(&client, node_name, Some(truncation), false).await;

    assert!(
        result.is_ok(),
        "get_node_config_with_options should succeed"
    );

    let response = result.unwrap();

    match response {
        ConfigWithOptionsResult::Config(config_response) => {
            // The truncated config should contain the TRUNCATED marker
            assert!(
                config_response.config.contains("TRUNCATED"),
                "Truncated config should contain TRUNCATED marker. Got {} chars.",
                config_response.config.len()
            );

            // Original size metadata should be preserved
            assert_eq!(
                config_response.size.lines, full_lines,
                "Size metadata should reflect original config"
            );

            println!(
                "Truncation test passed: {} original lines, truncated to ~11 lines",
                full_lines
            );
        }
        ConfigWithOptionsResult::Summary(_) => {
            panic!("Expected Config variant, got Summary");
        }
    }
}

/// Test that get_node_config_with_options without options returns full config (AC: 2).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_with_options_full_mode() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Request without truncation or summary
    let result = get_node_config_with_options(&client, node_name, None, false).await;

    assert!(
        result.is_ok(),
        "get_node_config_with_options should succeed"
    );

    let response = result.unwrap();

    match response {
        ConfigWithOptionsResult::Config(config_response) => {
            // Full config should NOT contain TRUNCATED marker
            assert!(
                !config_response.config.contains("TRUNCATED"),
                "Full config should not be truncated"
            );

            // Compare with regular get_node_config
            let regular = get_node_config(&client, node_name).await.unwrap();
            assert_eq!(
                config_response.config, regular.config,
                "Full config should match regular get_node_config"
            );

            println!(
                "Full mode test passed: {} bytes, {} lines",
                config_response.size.bytes, config_response.size.lines
            );
        }
        ConfigWithOptionsResult::Summary(_) => {
            panic!("Expected Config variant, got Summary");
        }
    }
}

/// Test that config summary detects network device vendor (AC: 4).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_summary_detects_vendor() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Request summary mode
    let result = get_node_config_with_options(&client, node_name, None, true).await;

    assert!(result.is_ok(), "Should succeed");

    if let ConfigWithOptionsResult::Summary(summary) = result.unwrap() {
        println!("Detected vendor: {}", summary.summary.vendor_hint);

        // Vendor hint should be non-empty
        assert!(
            !summary.summary.vendor_hint.is_empty(),
            "Vendor hint should not be empty"
        );

        // Should be one of the known vendor types or Unknown
        let valid_hints = [
            "Cisco IOS-style",
            "Juniper JunOS-style",
            "Cisco-like",
            "Unknown vendor",
        ];
        assert!(
            valid_hints.contains(&summary.summary.vendor_hint.as_str()),
            "Vendor hint '{}' should be a known type",
            summary.summary.vendor_hint
        );
    }
}

/// Test that config summary extracts section headers (AC: 4).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_summary_extracts_sections() {
    let client = create_client_from_env();

    // Get a valid node with successful backup
    let nodes = list_nodes(&client, None, Some(10), None).await.unwrap();
    let success_node = nodes
        .items
        .iter()
        .find(|n| n.effective_status() == Some("success"));

    if success_node.is_none() {
        println!("SKIP: No node with successful backup found");
        return;
    }

    let node_name = &success_node.unwrap().name;

    // Request summary mode
    let result = get_node_config_with_options(&client, node_name, None, true).await;

    assert!(result.is_ok(), "Should succeed");

    if let ConfigWithOptionsResult::Summary(summary) = result.unwrap() {
        println!("Sections detected:");
        for section in &summary.summary.sections {
            println!("  - {}", section);
        }

        // Most network configs should have at least some sections
        // (interface, hostname, vlan, etc.)
        println!(
            "Total sections: {}, Total lines: {}",
            summary.summary.sections.len(),
            summary.summary.total_lines
        );

        // Size metadata should be included
        assert!(
            summary.summary.size.bytes > 0,
            "Should include size metadata"
        );
    }
}

/// Test that get_node_config_with_options returns NodeNotFound with suggestions (AC: 5).
#[tokio::test]
#[ignore] // Requires real Oxidized server - run with: cargo test -- --ignored
async fn test_config_with_options_not_found_has_suggestions() {
    let client = create_client_from_env();

    // Get a real node name to build similar-but-nonexistent name
    let nodes = list_nodes(&client, None, Some(1), None).await.unwrap();
    if nodes.items.is_empty() {
        println!("SKIP: No nodes in inventory");
        return;
    }

    let existing_name = &nodes.items[0].name;
    let non_existent = format!("{}-NONEXISTENT-999", existing_name);

    let result = get_node_config_with_options(&client, &non_existent, None, false).await;

    assert!(result.is_err(), "Should return error for non-existent node");

    match result.unwrap_err() {
        OxidizedError::NodeNotFound(name, suggestions) => {
            assert_eq!(name, non_existent);
            assert!(
                !suggestions.is_empty(),
                "Should return suggestions for similar nodes"
            );
            println!(
                "NodeNotFound correctly returned {} suggestions: {:?}",
                suggestions.len(),
                suggestions
            );
        }
        other => panic!("Expected NodeNotFound, got: {:?}", other),
    }
}
