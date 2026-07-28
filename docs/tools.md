# Tools Reference

All v2 tools return both concise text and MCP `structuredContent`. Discovery
includes an output schema and annotations identifying read-only and mutating
operations. Existing `oxidized://` resources remain supported, but tools are
recommended for typed discovery and composition.

## Read tools

| Tool | Parameters | Purpose |
|------|------------|---------|
| `list_nodes` | `offset=0`, `limit=100`, `group?`, `name_pattern?`, `model?`, `status?` | Filter and paginate nodes in deterministic name order |
| `get_node` | `node` | Return one node and cache freshness |
| `get_node_config` | `node`, `mode=full`, `start_line?`, `end_line?`, `truncate_head?`, `truncate_tail?`, `force_refresh=false` | Read a current configuration |
| `list_config_versions` | `node`, `offset=0`, `limit=100` | List versions newest first |
| `get_config_version` | `node`, `oid` | Read a historical version |
| `diff_latest` | `node` | Compare the newest two versions |
| `diff_configs` | `node`, `version1`, `version2` | Compare selected versions |
| `search_configs` | See below | Search configurations |
| `get_backup_status` | `operation_id` | Poll a tracked backup |

`get_node_config` modes are:

- `full`: return the configuration, optionally preserving a requested number
  of head and tail lines.
- `summary`: return detected configuration sections and size information.
- `lines`: return the inclusive, one-based `start_line`/`end_line` range.

`force_refresh=true` bypasses the configuration cache. Every cache-bearing
response includes `metadata.cache_hit` and `metadata.fresh`.

## Searching configurations

`search_configs` accepts:

| Parameter | Default | Limit | Meaning |
|-----------|---------|-------|---------|
| `pattern` | required | non-empty | Rust regex, or plain text when `literal=true` |
| `nodes` | all | existing nodes | Optional search scope |
| `case_sensitive` | `true` | — | Case-sensitive matching |
| `literal` | `false` | — | Escape regex metacharacters |
| `context_before` | `1` | 0–50 | Context lines before a match |
| `context_after` | `1` | 0–50 | Context lines after a match |
| `limit_per_node` | none | 1–1000 | Matching-line cap per node |
| `offset` | `0` | non-negative | Global matching-line offset |
| `limit` | `100` | 1–1000 | Global matching-line page size |

Results are ordered by node name and line number. Adjacent matching lines are
merged into one block without repeating context. Pagination metadata includes
`total_matches`, `shown_matches`, `offset`, `limit`, and `has_more`.

The counters have distinct meanings:

- `nodes_searched`: valid nodes in the requested scope.
- `configs_fetched`: configurations downloaded after server-side prefiltering.
- `nodes_with_matches`: nodes with matches before global pagination.
- `nodes_returned`: nodes represented on this page.

If Oxidized's optional `conf_search` endpoint is unavailable, the tool warns and
falls back to fetching the scoped configurations. A successful prefilter with
zero candidates is treated as a trustworthy zero-match result.

Example:

```json
{
  "name": "search_configs",
  "arguments": {
    "pattern": "authentication-key",
    "literal": true,
    "context_before": 2,
    "context_after": 2,
    "offset": 0,
    "limit": 50,
    "limit_per_node": 10
  }
}
```

Search runs against raw configuration text, then masks returned match and
context lines. This allows callers to find secret directives without receiving
the values.

## Tracked backups

### fetch_node_config

Parameters are `node`, `wait=false`, and `timeout_seconds=60` (1–300).
The response contains an operation ID, baseline and latest backup metadata,
completion state, Oxidized status, and `mtime_changed`.

With `wait=false`, poll using `get_backup_status`:

```json
{
  "name": "fetch_node_config",
  "arguments": {"node": "router-core-01", "wait": false}
}
```

```json
{
  "name": "get_backup_status",
  "arguments": {"operation_id": "backup-operation-id"}
}
```

With `wait=true`, the call polls until the new run succeeds, fails, or times
out. A newer completed run counts as completion even if its configuration and
`mtime` are unchanged. Pending nodes bypass the configuration cache.

### fetch_node_configs

Queues 1–20 unique nodes. `concurrency` defaults to 5 and must be 1–10.
`wait` and `timeout_seconds` have the same meaning as the single-node tool.
Operations and aggregate completed/failed/pending counts are returned in
deterministic node order.

## Other write tools

| Tool | Parameters | Purpose |
|------|------------|---------|
| `prioritize_node` | `node` | Move a node to the front of the Oxidized queue |
| `reload_sources` | none | Reload source inventory and invalidate caches |

## Secret masking

Configuration-bearing responses include:

```json
{
  "redaction": {
    "enabled": true,
    "replacement_count": 2
  }
}
```

Diffs are computed from raw configurations, then their changed lines are
masked. Consequently, a secret-only change remains visible as a deletion and
addition containing `<redacted>`.

Set `OXIDIZED_REDACT_SECRETS=false` only on an administratively controlled
server that requires raw output. There is deliberately no per-tool bypass.
