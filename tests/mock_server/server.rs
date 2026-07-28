//! MockOxidizedServer - wiremock-based mock server reproducing Oxidized-web quirks.
//!
//! This server reproduces the following Oxidized-web 0.18.0 API behaviors:
//! - HTTP 500 for NodeNotFound (not 404)
//! - Nested `author` object in versions
//! - Stats endpoint returns 200 with garbage Ruby objects (bug in oxidized-web)
//! - conf_search POST returns HTML
//! - Node.last nested object for status

use super::fixtures::{MockNode, MockVersion};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Generate a random hex address for Ruby object string simulation.
fn rand_addr() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Simple pseudo-random based on current time nanos
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    0x0000_7000_0000_0000 | (nanos & 0x0000_0FFF_FFFF_FFFF)
}

/// Mock server that reproduces Oxidized-web API quirks.
pub struct MockOxidizedServer {
    inner: MockServer,
    nodes: Vec<MockNode>,
    versions: HashMap<String, Vec<MockVersion>>,
    configs: HashMap<String, String>,
    node_show_sequences: HashMap<String, Vec<MockNode>>,
    config_sequences: HashMap<String, Vec<String>>,
    version_configs: HashMap<(String, String), String>,
}

impl MockOxidizedServer {
    /// Start a new mock server with automatic port allocation.
    pub async fn start() -> Self {
        let inner = MockServer::start().await;
        Self {
            inner,
            nodes: vec![],
            versions: HashMap::new(),
            configs: HashMap::new(),
            node_show_sequences: HashMap::new(),
            config_sequences: HashMap::new(),
            version_configs: HashMap::new(),
        }
    }

    /// Get the server URI (e.g., `http://127.0.0.1:12345`).
    pub fn uri(&self) -> String {
        self.inner.uri()
    }

    /// Configure nodes to return from `/nodes.json`.
    #[must_use]
    pub fn with_nodes(mut self, nodes: Vec<MockNode>) -> Self {
        self.nodes = nodes;
        self
    }

    /// Configure versions for a specific node.
    #[must_use]
    pub fn with_versions(mut self, node: &str, versions: Vec<MockVersion>) -> Self {
        self.versions.insert(node.to_string(), versions);
        self
    }

    /// Configure configs for nodes.
    #[must_use]
    pub fn with_configs(mut self, configs: HashMap<String, String>) -> Self {
        self.configs = configs;
        self
    }

    /// Return successive node states from `/node/show/{name}.json`.
    ///
    /// The last state is repeated after the sequence is exhausted. This models
    /// pending, completed, failed, unchanged, and changed backup runs.
    #[must_use]
    pub fn with_node_show_sequence(mut self, node: &str, states: Vec<MockNode>) -> Self {
        assert!(!states.is_empty(), "node state sequence must not be empty");
        self.node_show_sequences.insert(node.to_string(), states);
        self
    }

    /// Return successive current configurations, repeating the final value.
    #[must_use]
    pub fn with_config_sequence(mut self, node: &str, configs: Vec<String>) -> Self {
        assert!(
            !configs.is_empty(),
            "configuration sequence must not be empty"
        );
        self.config_sequences.insert(node.to_string(), configs);
        self
    }

    /// Configure the exact content returned for one historical version.
    #[must_use]
    pub fn with_version_config(mut self, node: &str, oid: &str, config: &str) -> Self {
        self.version_configs
            .insert((node.to_string(), oid.to_string()), config.to_string());
        self
    }

    /// Mount all registered endpoints to the mock server.
    ///
    /// Call this after configuring nodes/versions/configs.
    pub async fn mount_all(&self) {
        self.mount_nodes_endpoint().await;
        self.mount_stats_endpoint().await;
        self.mount_node_show_endpoints().await;
        self.mount_node_config_endpoints().await;
        self.mount_node_versions_endpoints().await;
        self.mount_node_version_view_endpoints().await;
        self.mount_node_next_endpoints().await;
        self.mount_reload_endpoint().await;
        self.mount_conf_search_endpoint().await;
    }

    /// Mount GET /nodes.json - returns all nodes
    async fn mount_nodes_endpoint(&self) {
        Mock::given(method("GET"))
            .and(path("/nodes.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&self.nodes))
            .mount(&self.inner)
            .await;
    }

    /// Mount GET /nodes/stats.json - returns 200 with garbage Ruby objects (API quirk)
    ///
    /// Real API returns HTTP 200 but with unserialized Ruby objects like:
    /// `{"node-name":"#<Oxidized::Node::Stats:0x000070dd3a080b28>"}`
    ///
    /// This is a known bug in oxidized-web. Real code uses Stats::from_nodes()
    /// to compute stats from /nodes.json instead.
    async fn mount_stats_endpoint(&self) {
        // Build garbage response matching real API behavior
        let nodes = self.nodes.clone();
        Mock::given(method("GET"))
            .and(path("/nodes/stats.json"))
            .respond_with(move |_req: &wiremock::Request| {
                // Return garbage Ruby object strings like real API
                let garbage: std::collections::HashMap<String, String> = nodes
                    .iter()
                    .map(|n| {
                        (
                            n.name.clone(),
                            format!("#<Oxidized::Node::Stats:0x{:016x}>", rand_addr()),
                        )
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(&garbage)
            })
            .mount(&self.inner)
            .await;
    }

    /// Mount GET /node/show/{name}.json endpoints
    ///
    /// - Valid nodes: return 200 with node details
    /// - Invalid nodes: return 500 (API quirk - not 404!)
    async fn mount_node_show_endpoints(&self) {
        // Mount endpoints for each known node
        for node in &self.nodes {
            let path_str = format!("/node/show/{}.json", node.name);
            if let Some(states) = self.node_show_sequences.get(&node.name) {
                let states = states.clone();
                let calls = Arc::new(AtomicUsize::new(0));
                Mock::given(method("GET"))
                    .and(path(&path_str))
                    .respond_with(move |_request: &wiremock::Request| {
                        let index = calls.fetch_add(1, Ordering::Relaxed);
                        let state = &states[index.min(states.len() - 1)];
                        ResponseTemplate::new(200).set_body_json(state)
                    })
                    .mount(&self.inner)
                    .await;
            } else {
                Mock::given(method("GET"))
                    .and(path(&path_str))
                    .respond_with(ResponseTemplate::new(200).set_body_json(node))
                    .mount(&self.inner)
                    .await;
            }
        }

        // Mount catch-all for unknown nodes - HTTP 500 (API quirk!)
        // This must have lower priority, so mount it with expect(0) to avoid matching known nodes
        // Actually, wiremock matches in reverse order of mounting, so we need a different approach
        // We'll use a regex that doesn't match our known nodes
        let known_names: Vec<String> = self.nodes.iter().map(|n| n.name.clone()).collect();

        // Mount catch-all FIRST (will have lower priority)
        // We use a response that includes the node name extracted from path
        Mock::given(method("GET"))
            .and(path_regex(r"^/node/show/[^/]+\.json$"))
            .respond_with(move |req: &wiremock::Request| {
                // Extract node name from path
                let path = req.url.path();
                let name = path
                    .strip_prefix("/node/show/")
                    .and_then(|s| s.strip_suffix(".json"))
                    .unwrap_or("unknown");

                // If this is a known node, return 200 (this shouldn't happen with proper mocking)
                if known_names.contains(&name.to_string()) {
                    // This shouldn't be reached as specific mocks take precedence
                    ResponseTemplate::new(200)
                } else {
                    // HTTP 500 with Ruby-style error body (API quirk!)
                    let error_body = format!(
                        "unable to find '{}'\n\nRuby stack trace would go here...",
                        name
                    );
                    ResponseTemplate::new(500).set_body_string(error_body)
                }
            })
            .mount(&self.inner)
            .await;
    }

    /// Mount GET /node/fetch/{group}/{name} - get current config (no .json extension)
    async fn mount_node_config_endpoints(&self) {
        for (node_name, config) in &self.configs {
            let full_name = self
                .nodes
                .iter()
                .find(|node| &node.name == node_name)
                .map(|node| node.full_name.as_str())
                .unwrap_or(node_name);
            let path_str = format!("/node/fetch/{}", full_name);
            // Oxidized returns raw text, not JSON
            if let Some(configs) = self.config_sequences.get(node_name) {
                let configs = configs.clone();
                let calls = Arc::new(AtomicUsize::new(0));
                Mock::given(method("GET"))
                    .and(path(&path_str))
                    .respond_with(move |_request: &wiremock::Request| {
                        let index = calls.fetch_add(1, Ordering::Relaxed);
                        ResponseTemplate::new(200)
                            .set_body_string(&configs[index.min(configs.len() - 1)])
                            .insert_header("content-type", "text/plain")
                    })
                    .mount(&self.inner)
                    .await;
            } else {
                Mock::given(method("GET"))
                    .and(path(&path_str))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_string(config)
                            .insert_header("content-type", "text/plain"),
                    )
                    .mount(&self.inner)
                    .await;
            }
        }

        // Catch-all for unknown nodes returns 500 (same as show)
        let known_nodes: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| self.configs.contains_key(&node.name))
            .map(|node| node.full_name.clone())
            .collect();
        Mock::given(method("GET"))
            .and(path_regex(r"^/node/fetch/.+$"))
            .respond_with(move |req: &wiremock::Request| {
                let path = req.url.path();
                let full_name = path.strip_prefix("/node/fetch/").unwrap_or("unknown");

                if known_nodes.contains(&full_name.to_string()) {
                    ResponseTemplate::new(200)
                } else {
                    let error_body = format!("unable to find '{}'", full_name);
                    ResponseTemplate::new(500).set_body_string(error_body)
                }
            })
            .mount(&self.inner)
            .await;
    }

    /// Mount GET /node/version.json?node_full={group}/{name} - get version list
    async fn mount_node_versions_endpoints(&self) {
        for (node_name, versions) in &self.versions {
            // Find the node to get its full_name
            if let Some(node) = self.nodes.iter().find(|n| &n.name == node_name) {
                // Oxidized uses node_full parameter
                Mock::given(method("GET"))
                    .and(path("/node/version.json"))
                    .and(wiremock::matchers::query_param(
                        "node_full",
                        &node.full_name,
                    ))
                    .respond_with(ResponseTemplate::new(200).set_body_json(versions))
                    .mount(&self.inner)
                    .await;
            }
        }
    }

    /// Mount GET /node/version/view.json?node={name}&group={group}&oid={oid}
    ///
    /// Returns JSON array of lines (not raw text).
    async fn mount_node_version_view_endpoints(&self) {
        for (node_name, versions) in &self.versions {
            if let Some(node) = self.nodes.iter().find(|n| &n.name == node_name) {
                for version in versions {
                    // Get config content for this version
                    let config = self
                        .version_configs
                        .get(&(node_name.clone(), version.oid.clone()))
                        .or_else(|| self.configs.get(node_name))
                        .map(|config| config.as_str())
                        .unwrap_or("no config");

                    // Oxidized returns JSON array of lines
                    // The real endpoint retains line terminators; the client
                    // joins the returned array verbatim.
                    let lines: Vec<&str> = config.split_inclusive('\n').collect();

                    Mock::given(method("GET"))
                        .and(path("/node/version/view.json"))
                        .and(wiremock::matchers::query_param("node", &node.name))
                        .and(wiremock::matchers::query_param("group", &node.group))
                        .and(wiremock::matchers::query_param("oid", &version.oid))
                        .respond_with(ResponseTemplate::new(200).set_body_json(&lines))
                        .mount(&self.inner)
                        .await;
                }
            }
        }

        // Invalid OID returns ["version not found"]
        Mock::given(method("GET"))
            .and(path("/node/version/view.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["version not found"]))
            .mount(&self.inner)
            .await;
    }

    /// Mount GET /node/next/{name}.json - trigger backup / prioritize
    ///
    /// Oxidized uses GET (not PUT) for these operations.
    async fn mount_node_next_endpoints(&self) {
        for node in &self.nodes {
            let path_str = format!("/node/next/{}.json", node.name);
            Mock::given(method("GET"))
                .and(path(&path_str))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
                )
                .mount(&self.inner)
                .await;
        }

        // Catch-all for unknown nodes
        Mock::given(method("GET"))
            .and(path_regex(r"^/node/next/[^/]+\.json$"))
            .respond_with(|req: &wiremock::Request| {
                let path = req.url.path();
                let name = path
                    .strip_prefix("/node/next/")
                    .and_then(|s| s.strip_suffix(".json"))
                    .unwrap_or("unknown");
                let error_body = format!("unable to find '{}'", name);
                ResponseTemplate::new(500).set_body_string(error_body)
            })
            .mount(&self.inner)
            .await;
    }

    /// Mount GET /reload?format=json - reload source inventory
    async fn mount_reload_endpoint(&self) {
        // Client uses GET /reload?format=json
        Mock::given(method("GET"))
            .and(path("/reload"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .mount(&self.inner)
            .await;
    }

    /// Mount POST /nodes/conf_search - returns HTML (API quirk!)
    ///
    /// Oxidized-web returns HTML with `<table>` structure, not JSON.
    async fn mount_conf_search_endpoint(&self) {
        let nodes = self.nodes.clone();
        let configs = self.configs.clone();

        Mock::given(method("POST"))
            .and(path("/nodes/conf_search"))
            .respond_with(move |req: &wiremock::Request| {
                // Parse form data to get search pattern
                let body_str = String::from_utf8_lossy(req.body.as_slice());

                // Extract search pattern from form data (search_in_conf_textbox=pattern)
                let pattern = body_str
                    .split('&')
                    .find(|p| p.starts_with("search_in_conf_textbox="))
                    .and_then(|p| p.strip_prefix("search_in_conf_textbox="))
                    .map(|p| urlencoding::decode(p).unwrap_or_default().to_string())
                    .unwrap_or_default();

                // Find nodes that match the pattern in their config
                let matching_nodes: Vec<&MockNode> = nodes
                    .iter()
                    .filter(|n| {
                        if let Some(config) = configs.get(&n.name) {
                            config.to_lowercase().contains(&pattern.to_lowercase())
                        } else {
                            false
                        }
                    })
                    .collect();

                // Build HTML response (API quirk!)
                let mut html = String::from("<html><body><table>");
                for node in &matching_nodes {
                    html.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td></tr>",
                        node.name, node.group
                    ));
                }
                html.push_str("</table></body></html>");

                ResponseTemplate::new(200)
                    .set_body_string(html)
                    .insert_header("content-type", "text/html")
            })
            .mount(&self.inner)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_server::fixtures::{default_configs, default_nodes, default_versions};

    #[tokio::test]
    async fn test_mock_server_starts() {
        let mock = MockOxidizedServer::start().await;
        assert!(mock.uri().starts_with("http://"));
    }

    #[tokio::test]
    async fn test_mock_returns_500_for_unknown_node() {
        let mock = MockOxidizedServer::start()
            .await
            .with_nodes(default_nodes());
        mock.mount_all().await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/node/show/nonexistent-node.json", mock.uri()))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 500);
        let body = resp.text().await.unwrap();
        assert!(body.contains("unable to find 'nonexistent-node'"));
    }

    #[tokio::test]
    async fn test_mock_versions_have_nested_author() {
        let mock = MockOxidizedServer::start()
            .await
            .with_nodes(default_nodes())
            .with_versions("router-1", default_versions());
        mock.mount_all().await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "{}/node/version.json?node_full=network/router-1",
                mock.uri()
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();

        // Check nested author object
        assert!(body[0]["author"]["name"].is_string());
        assert!(body[0]["author"]["email"].is_string());
        assert!(body[0]["author"]["time"].is_string());
    }

    #[tokio::test]
    async fn test_mock_stats_endpoint_returns_garbage() {
        let mock = MockOxidizedServer::start()
            .await
            .with_nodes(default_nodes());
        mock.mount_all().await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/nodes/stats.json", mock.uri()))
            .send()
            .await
            .unwrap();

        // Real API returns 200 but with garbage Ruby objects (bug in oxidized-web)
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();

        // Response contains node names as keys with garbage Ruby object strings
        assert!(body["router-1"].is_string());
        let garbage = body["router-1"].as_str().unwrap();
        assert!(
            garbage.starts_with("#<Oxidized::Node::Stats:"),
            "Should contain Ruby object string: {}",
            garbage
        );
    }

    #[tokio::test]
    async fn test_mock_conf_search_returns_html() {
        let mock = MockOxidizedServer::start()
            .await
            .with_nodes(default_nodes())
            .with_configs(default_configs());
        mock.mount_all().await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/nodes/conf_search", mock.uri()))
            .body("search_in_conf_textbox=hostname")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        let body = resp.text().await.unwrap();
        // Verify HTML structure (API quirk)
        assert!(
            body.contains("<table>"),
            "Response should contain HTML table"
        );
        assert!(body.contains("<td>"), "Response should contain table cells");
        // router-1 config contains "hostname", should be in results
        assert!(
            body.contains("router-1"),
            "Should find router-1 which has 'hostname' in config"
        );
    }

    #[tokio::test]
    async fn test_mock_node_has_nested_last_object() {
        let mock = MockOxidizedServer::start()
            .await
            .with_nodes(default_nodes());
        mock.mount_all().await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/node/show/router-1.json", mock.uri()))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();

        // Check nested last object
        assert!(body["last"]["status"].is_string());
        assert!(body["last"]["start"].is_string());
        assert!(body["last"]["end"].is_string());
    }
}
