# Resources Reference

The v1 resource URIs remain available in v2. New integrations should generally
prefer the equivalent read tools because tools advertise input/output schemas
and return typed `structuredContent`.

| Resource URI | Equivalent tool | Description |
|--------------|-----------------|-------------|
| `oxidized://nodes` | `list_nodes` | Paginated inventory |
| `oxidized://node/{name}` | `get_node` | Node details |
| `oxidized://node/{name}/config` | `get_node_config` | Current configuration |
| `oxidized://node/{name}/versions` | `list_config_versions` | Version history |
| `oxidized://node/{name}/versions/{oid}` | `get_config_version` | Historical configuration |
| `oxidized://stats` | — | Global statistics |

Node names in URI paths should be URL encoded.

## Current configuration options

The current configuration resource retains its query options:

```text
oxidized://node/router-core-01/config
oxidized://node/router-core-01/config?summary=true
oxidized://node/router-core-01/config?truncate=true&truncate_head=100&truncate_tail=50
```

`summary=true` returns detected section and size information.
`truncate=true` preserves head and tail lines, with defaults of 500 and 100.

## Freshness metadata

Cache-bearing resource responses include:

```json
{
  "metadata": {
    "cache_hit": false,
    "fresh": true
  }
}
```

`fresh=true` means the value was fetched from Oxidized for that request.
`cache_hit=true` means it came from the MCP server's in-memory cache.
Historical version content and version lists are fetched directly and report a
fresh cache miss.

Cache TTLs are:

| Data | TTL |
|------|-----|
| Nodes and node details | 5 minutes |
| Current configuration | 2 minutes |
| Statistics | 30 seconds |

Tracked pending backups bypass current-configuration caching. Successful
completion invalidates the affected entry. `reload_sources` invalidates all
inventory-related caches.

## Secret masking

Current configuration, summary/truncation inputs, and historical version
content are masked by default and include:

```json
{
  "redaction": {
    "enabled": true,
    "replacement_count": 1
  }
}
```

Masking covers common Cisco and Junos SNMP communities, local and enable
passwords, TACACS/RADIUS keys, routing authentication, pre-shared keys, private
keys, and secret-data markers. It occurs before configuration-bearing resource
content is serialized.

The v2 migration changes the default returned configuration text. Only a server
administrator can restore raw output by setting
`OXIDIZED_REDACT_SECRETS=false`; resources do not accept a per-request bypass.

## Errors

Unknown nodes return actionable errors with similar-node suggestions where
possible. Unknown resource URIs and invalid version OIDs are reported as MCP
invalid-parameter errors.
