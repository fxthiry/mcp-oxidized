# mcp-oxidized

[![CI](https://github.com/fxthiry/mcp-oxidized/actions/workflows/ci.yml/badge.svg)](https://github.com/fxthiry/mcp-oxidized/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/mcp-oxidized.svg)](https://crates.io/crates/mcp-oxidized)
[![codecov](https://codecov.io/gh/fxthiry/mcp-oxidized/branch/main/graph/badge.svg)](https://codecov.io/gh/fxthiry/mcp-oxidized)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)

> MCP server exposing Oxidized network configuration backups to AI assistants with tracked freshness, structured tool output, and secret masking by default.

mcp-oxidized is a [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server that connects AI assistants (Claude Desktop, Cursor, Zed, Windsurf) to your [Oxidized](https://github.com/ytti/oxidized) network device configuration backup system.

**Key Differentiator:** When something goes wrong, you get structured error messages that LLMs can understand and act upon - not cryptic stack traces.

## Quick Start

Get up and running in less than 5 minutes:

### 1. Prerequisites

- **Oxidized 0.35.0+** with **oxidized-web 0.18.0+** running and accessible
- An MCP-compatible client (Claude Desktop, Cursor, Zed, Windsurf)

> **Note**: The REST API is provided by [oxidized-web](https://github.com/ytti/oxidized-web), a separate Ruby gem from Oxidized itself.

### 2. Installation

**Option A: Download binary** (recommended)

Download the latest binary for your platform from [Releases](https://github.com/fxthiry/mcp-oxidized/releases).

**Option B: Build from source**

```bash
cargo install mcp-oxidized
# or
git clone https://github.com/fxthiry/mcp-oxidized.git
cd mcp-oxidized
cargo build --release
```

### 3. Configuration

Add to your MCP client config. Example for Claude Desktop (`~/.config/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "oxidized": {
      "command": "/path/to/mcp-oxidized",
      "env": {
        "OXIDIZED_URL": "https://your-oxidized-server:8888",
        "OXIDIZED_USER": "admin",
        "OXIDIZED_PASSWORD": "your-password"
      }
    }
  }
}
```

**Zero-config mode:** If no env vars are set, defaults to `http://localhost:8888` with no authentication.

📖 **[Full Configuration Guide](docs/configuration.md)** - All environment variables, MCP client configs (Cursor, Zed, Windsurf), SSL options, custom headers, and security best practices.

## Tools

| Tool | Parameters | Description |
|------|------------|-------------|
| `list_nodes` | `offset?`, `limit?`, `group?`, `name_pattern?`, `model?`, `status?` | List and filter nodes |
| `get_node` | `node` | Get node details and freshness metadata |
| `get_node_config` | `node`, `mode?`, line/truncation options, `force_refresh?` | Read a current configuration |
| `list_config_versions` | `node`, `offset?`, `limit?` | List historical versions |
| `get_config_version` | `node`, `oid` | Read a historical configuration |
| `diff_latest` | `node` | Compare the newest two versions |
| `diff_configs` | `node`, `version1`, `version2` | Compare any two versions |
| `search_configs` | `pattern` and search/pagination options | Search raw configs and return masked matches |
| `fetch_node_config` | `node`, `wait?`, `timeout_seconds?` | Queue and track an immediate backup |
| `get_backup_status` | `operation_id` | Poll a backup operation |
| `fetch_node_configs` | `nodes`, `wait?`, `timeout_seconds?`, `concurrency?` | Queue up to 20 backups |
| `prioritize_node` | `node` | Move a node to the front of the backup queue |
| `reload_sources` | _(none)_ | Reload Oxidized source inventory (new devices become available) |

Every tool returns concise text plus typed MCP `structuredContent`; its discovery entry includes an output schema and read/write annotations. The `oxidized://` resources remain available for compatibility.

## Resources

| Resource URI | Description |
|--------------|-------------|
| `oxidized://nodes` | List all nodes with pagination (`offset`, `limit`, `group`) |
| `oxidized://node/{name}` | Node details (model, status, last backup time) |
| `oxidized://node/{name}/config` | Current configuration (with `truncate`, `summary` options for large configs) |
| `oxidized://node/{name}/versions` | Configuration version history |
| `oxidized://node/{name}/versions/{oid}` | Specific historical version content |
| `oxidized://stats` | Global backup statistics |

## Actionable Errors

When something goes wrong, mcp-oxidized provides structured error messages optimized for AI assistants:

```
[Error] Node 'SW-Unknown' not found.
[Context] Search performed in Oxidized inventory.
[Suggestions] Similar nodes: SW-Core-01, SW-Access-02.
[Next Step] Use 'oxidized://nodes' to list all available nodes.
```

This format helps LLMs understand what went wrong and suggest corrections automatically.

## Example Usage

Ask your AI assistant:

- "List all network devices in Oxidized"
- "Show me the configuration of router-core-01"
- "Compare the last two versions of switch-access-02"
- "Find all devices with SNMP community configuration"
- "Trigger a backup of firewall-edge-01 now"

---

## Security Considerations

Version 2 masks common configuration secrets by default in current and historical configurations, summaries, searches, diffs, truncation, and resources. Results include:

```json
{"redaction":{"enabled":true,"replacement_count":3}}
```

Search matching and diff calculation use raw configurations first, so a secret-only change is still visible as a changed line without revealing either value.

For a tightly controlled administrative deployment that requires raw backups, set `OXIDIZED_REDACT_SECRETS=false` on the MCP server. This is a server-wide switch, not a per-call override, and startup emits a warning. Upgrading from v1 therefore intentionally changes configuration output.

**Recommendations:**

- **Keep masking enabled** - Treat `OXIDIZED_REDACT_SECRETS=false` as privileged raw-data access
- **Review what you share** - Configurations still contain sensitive topology, addressing, and policy data after secret masking
- **Use with trusted LLM providers** - Ensure your organization's policies allow sending network configuration data to the AI service you're using
- **Consider data residency** - Some LLM providers may process data in different jurisdictions
- **Limit scope when possible** - Use node filtering in searches rather than querying all configurations

mcp-oxidized itself does not log or transmit configuration content beyond what the MCP protocol requires for your AI assistant to function.

---

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, testing, and PR guidelines.

## Documentation

- [Tools Reference](docs/tools.md) - Detailed tool documentation with examples
- [Resources Reference](docs/resources.md) - Resource URI patterns and response formats
- [Configuration Guide](docs/configuration.md) - All environment variables and options
- [Troubleshooting](docs/troubleshooting.md) - Common errors and solutions

## License

Apache License 2.0 - see [LICENSE](LICENSE) for details.
