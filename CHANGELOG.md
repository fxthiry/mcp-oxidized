# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.0.0] - 2026-07-28

### Security

- Mask common Cisco and Junos configuration secrets by default across current
  and historical configurations, summaries, truncation, searches, diffs, and
  resources.
- Add `OXIDIZED_REDACT_SECRETS=false` as an administrator-only, server-wide raw
  output switch with a startup warning.
- Compute searches and diffs from raw text before masking their returned lines,
  preserving evidence of secret-only changes without exposing values.

### Added

- Add resource-equivalent read tools: `list_nodes`, `get_node`,
  `get_node_config`, `list_config_versions`, `get_config_version`, and
  `diff_latest`.
- Add structured MCP output schemas, `structuredContent`, and correct read/write
  annotations for every tool.
- Add tracked backup operations, `get_backup_status`, optional waiting, batch
  backup requests, baseline/latest run metadata, and `mtime_changed`.
- Add deterministic search pagination, literal matching, configurable context,
  adjacent-match merging, per-node limits, model/backup/cache metadata, and
  distinct scope/fetch/match/return counters.
- Add explicit `fresh` cache metadata to resource and tool responses.

### Changed

- Treat an available Oxidized configuration prefilter with no candidates as a
  real zero-match result; fall back only when the endpoint is unavailable.
- Pending backup nodes bypass the configuration cache, and newer completed
  runs count as complete even when configuration content is unchanged.
- Preserve all existing `oxidized://` resources and legacy tool names.

### Breaking

- Configuration-bearing output is masked by default. This intentional output
  compatibility change is the reason for the v2 major release.

## [1.2.0] - 2026-01-05

### Changed

- **URL Encoding** - All node names are now URL-encoded when calling Oxidized API endpoints, fixing issues with special characters (spaces, slashes, etc.)
- **Unified Dependencies** - Replaced `percent-encoding` with `urlencoding` crate (simpler API, fewer dependencies)
- **Tokio Optimization** - Reduced tokio features from `["full"]` to minimal required set (`rt-multi-thread`, `macros`, `sync`, `time`)

### Fixed

- **Panic Elimination** - Replaced all `.expect()` calls in production code with proper error handling:
  - `OxidizedClient::new()` → `OxidizedClient::try_new()` returning `Result`
  - Retry loop graceful fallback instead of panic
  - Semaphore acquisition in search_configs
- **Regex Caching** - Static regex compilation for `TD_REGEX` and `SECTION_REGEX` (performance improvement)

### Added

- 14 new unit tests for `fetch_node_config`, `prioritize_node`, and `reload_sources` tools
- Standardized test patterns with `expected_message()` helpers
- Unicode and JSON escaping test coverage

## [1.1.0] - 2025-12-23

### Added

- **SSL Verification Control** - New `OXIDIZED_SSL_VERIFY` environment variable to disable certificate verification for self-signed certificates (default: `true`, only affects HTTPS URLs)
- **Custom HTTP Headers** - New `OXIDIZED_HEADERS` environment variable for adding custom headers to all requests (format: `Header1:Value1,Header2:Value2`)
- Custom `Authorization` header takes precedence over Basic Auth credentials
- Startup warnings when SSL verification is disabled or custom Authorization overrides Basic Auth
- Improved error messages for SSL certificate failures with actionable suggestion to set `OXIDIZED_SSL_VERIFY=false`

## [1.0.1] - 2025-12-23

### Fixed

- Switch from OpenSSL to rustls for cross-platform binary builds (fixes Linux ARM64 musl build)

## [1.0.0] - 2025-12-23

### Initial Release

mcp-oxidized is an MCP server that exposes Oxidized network configuration backup capabilities to AI assistants (Claude Desktop, Cursor, Zed, Windsurf).

**Key Features:**

- 5 MCP Tools - Trigger backups, compare configs, search patterns across your network
- 6 MCP Resources - Discover nodes, view configurations, browse version history
- Actionable Errors - LLM-optimized error messages with suggestions and next steps
- Smart Caching - moka-based cache with automatic invalidation on write operations
- Large Config Handling - Truncation, summary mode, and token estimation for oversized configs
- Resilient Operations - Retry logic with exponential backoff for transient failures

**Compatibility:**

- Oxidized 0.35.0+ / Oxidized-web 0.18.0+
- MCP Stdio transport (Claude Desktop, Cursor, Zed, Windsurf)

### Added

#### Tools

- `fetch_node_config` - Trigger immediate backup of a node's configuration
- `prioritize_node` - Move a node to the front of the backup queue
- `reload_sources` - Reload Oxidized source inventory (new devices available immediately)
- `diff_configs` - Compare two configuration versions using Myers/LCS algorithm
- `search_configs` - Regex search across configurations with server-side pre-filter

#### Resources

- `oxidized://nodes` - List all nodes with pagination and group filtering
- `oxidized://node/{name}` - Node details (model, status, last backup time)
- `oxidized://node/{name}/config` - Current configuration with truncate/summary options
- `oxidized://node/{name}/versions` - Configuration version history
- `oxidized://node/{name}/versions/{oid}` - Specific historical version content
- `oxidized://stats` - Global backup statistics

#### Infrastructure

- Actionable error framework with `[Error]`, `[Context]`, `[Suggestions]`, `[Next Step]` format
- Cache with TTL: nodes (5min), config (2min), stats (30s)
- E2E test suite with wiremock mock server (runs in CI without real Oxidized)
- Integration tests for real Oxidized validation (`cargo test -- --ignored`)
- Code coverage with cargo-tarpaulin in CI
- cargo-dist for multi-platform binary releases

### Documentation

- README with quick start guide (< 5 minutes)
- CONTRIBUTING.md with development setup and PR guidelines
- docs/tools.md - Complete tool reference with examples
- docs/resources.md - Resource URI patterns and response formats
- docs/configuration.md - All environment variables and MCP client configs
- docs/troubleshooting.md - Common errors and solutions

[Unreleased]: https://github.com/fxthiry/mcp-oxidized/compare/v2.0.0...HEAD
[2.0.0]: https://github.com/fxthiry/mcp-oxidized/compare/v1.2.0...v2.0.0
[1.2.0]: https://github.com/fxthiry/mcp-oxidized/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/fxthiry/mcp-oxidized/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/fxthiry/mcp-oxidized/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/fxthiry/mcp-oxidized/releases/tag/v1.0.0
