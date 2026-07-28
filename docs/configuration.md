# Configuration Guide

This document describes all configuration options for mcp-oxidized.

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OXIDIZED_URL` | No | `http://localhost:8888` | Oxidized REST API base URL |
| `OXIDIZED_USER` | No | _(none)_ | Username for Basic Auth |
| `OXIDIZED_PASSWORD` | No | _(none)_ | Password for Basic Auth |
| `OXIDIZED_PASSWORD_FILE` | No | _(none)_ | Path to file containing password |
| `OXIDIZED_SSL_VERIFY` | No | `true` | SSL certificate verification |
| `OXIDIZED_HEADERS` | No | _(none)_ | Custom HTTP headers |
| `OXIDIZED_REDACT_SECRETS` | No | `true` | Mask secrets in configuration-bearing output |

### OXIDIZED_URL

The base URL of your Oxidized REST API (provided by oxidized-web).

```bash
export OXIDIZED_URL="http://oxidized.example.com:8888"
export OXIDIZED_URL="https://oxidized.example.com"  # HTTPS supported
```

### OXIDIZED_USER / OXIDIZED_PASSWORD

Credentials for HTTP Basic Authentication. Only required if your Oxidized instance requires authentication.

```bash
export OXIDIZED_USER="admin"
export OXIDIZED_PASSWORD="your-secret-password"
```

### OXIDIZED_PASSWORD_FILE

For enhanced security, you can store the password in a file instead of an environment variable. This is useful for:
- Docker secrets
- Kubernetes secrets mounted as files
- Avoiding password in shell history

```bash
export OXIDIZED_PASSWORD_FILE="/run/secrets/oxidized-password"
```

**Precedence:** If both `OXIDIZED_PASSWORD` and `OXIDIZED_PASSWORD_FILE` are set, `OXIDIZED_PASSWORD_FILE` takes precedence.

### OXIDIZED_SSL_VERIFY

Control SSL/TLS certificate verification for HTTPS connections. Default is `true` (certificates are verified).

> **Note:** This setting only affects HTTPS URLs. For HTTP URLs (e.g., `http://localhost:8888`), this setting is silently ignored because there is no SSL/TLS certificate to verify.

```bash
# Disable certificate verification (not recommended for production)
export OXIDIZED_SSL_VERIFY="false"
```

> **Security Warning:** Disabling SSL verification makes connections vulnerable to man-in-the-middle attacks. Only use `false` in development/testing environments or when connecting to servers with self-signed certificates that you trust.

When disabled with an HTTPS URL, mcp-oxidized logs a warning at startup:
```
WARN mcp_oxidized: SSL certificate verification disabled
```

If a connection fails due to SSL certificate issues, the error message will suggest setting `OXIDIZED_SSL_VERIFY=false`.

### OXIDIZED_HEADERS

Add custom HTTP headers to all requests. Useful for:
- API gateways requiring custom authentication
- Proxy authentication
- Custom request identification

**Format:** Comma-separated `Header:Value` pairs.

```bash
export OXIDIZED_HEADERS="X-Api-Key:your-api-key,X-Custom:value"
```

**Advanced: Authorization Header**

If you provide a custom `Authorization` header, it takes precedence over Basic Auth credentials:

```bash
# This Bearer token will be used instead of OXIDIZED_USER/PASSWORD
export OXIDIZED_HEADERS="Authorization:Bearer your-token"
export OXIDIZED_USER="admin"        # Ignored when custom Authorization is set
export OXIDIZED_PASSWORD="secret"   # Ignored when custom Authorization is set
```

When both are configured, mcp-oxidized logs a warning:
```
WARN mcp_oxidized: Custom Authorization header overrides OXIDIZED_USER/PASSWORD
```

**Edge cases:**
- Malformed headers are skipped with a warning
- Empty values are allowed (`X-Empty:`)
- Values can contain colons (`Authorization:Bearer token:with:colons`)

### OXIDIZED_REDACT_SECRETS

Controls centralized masking of secret-bearing configuration lines.

```bash
# Default and recommended
export OXIDIZED_REDACT_SECRETS=true

# Administrative raw-output mode; a startup warning is emitted
export OXIDIZED_REDACT_SECRETS=false
```

Masking applies to current and historical configurations, summaries,
truncation, searches, diffs, and resource responses. Search evaluates raw text
before masking. Diffs are calculated from raw versions and mask their changed
lines afterward. Raw output can only be enabled server-wide; callers cannot
disable masking for an individual request.

This default is new in v2.0.0 and intentionally changes the configuration
content returned by clients upgrading from v1.

---

## Configuration Precedence

Configuration values are resolved in this order (first match wins):

1. **MCP client env block** - Environment variables in your client's config file
2. **System environment** - Variables exported in your shell
3. **Built-in defaults** - `http://localhost:8888`, no authentication

---

## Zero-Config Mode

If no environment variables are set, mcp-oxidized defaults to:
- **URL:** `http://localhost:8888`
- **Authentication:** None

This is useful for local development or when Oxidized runs on the same machine.

---

## MCP Client Configuration

### Claude Desktop

**Linux/macOS:** `~/.config/Claude/claude_desktop_config.json`
**Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "oxidized": {
      "command": "/path/to/mcp-oxidized",
      "env": {
        "OXIDIZED_URL": "http://oxidized.example.com:8888",
        "OXIDIZED_USER": "admin",
        "OXIDIZED_PASSWORD": "your-password"
      }
    }
  }
}
```

### Cursor

**Config:** `~/.cursor/mcp.json`

```json
{
  "mcpServers": {
    "oxidized": {
      "command": "/path/to/mcp-oxidized",
      "env": {
        "OXIDIZED_URL": "http://oxidized.example.com:8888"
      }
    }
  }
}
```

### Zed

**Config:** `~/.config/zed/settings.json`

```json
{
  "context_servers": {
    "oxidized": {
      "command": {
        "path": "/path/to/mcp-oxidized",
        "env": {
          "OXIDIZED_URL": "http://oxidized.example.com:8888"
        }
      }
    }
  }
}
```

### Windsurf

**Config:** `~/.codeium/windsurf/mcp_config.json`

```json
{
  "mcpServers": {
    "oxidized": {
      "command": "/path/to/mcp-oxidized",
      "env": {
        "OXIDIZED_URL": "http://oxidized.example.com:8888"
      }
    }
  }
}
```

---

## Logging Configuration

mcp-oxidized uses the `tracing` framework. Control log levels via `RUST_LOG`:

```bash
# Default (info level)
export RUST_LOG=info

# Debug HTTP requests
export RUST_LOG=mcp_oxidized=debug

# Trace all details (verbose)
export RUST_LOG=mcp_oxidized=trace

# Only errors
export RUST_LOG=error
```

Logs are written to **stderr** (as per MCP protocol - stdout is reserved for JSON-RPC).

---

## Security Best Practices

### Avoid Password in Shell History

Use `OXIDIZED_PASSWORD_FILE`:

```bash
# Create secure password file
echo "your-password" > ~/.oxidized-password
chmod 600 ~/.oxidized-password

# Configure
export OXIDIZED_PASSWORD_FILE="$HOME/.oxidized-password"
```

### Docker Secrets

```yaml
# docker-compose.yml
services:
  mcp-oxidized:
    image: mcp-oxidized
    environment:
      - OXIDIZED_URL=http://oxidized:8888
      - OXIDIZED_USER=admin
      - OXIDIZED_PASSWORD_FILE=/run/secrets/oxidized-password
    secrets:
      - oxidized-password

secrets:
  oxidized-password:
    file: ./oxidized-password.txt
```

### Kubernetes Secrets

```yaml
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: mcp-oxidized
      env:
        - name: OXIDIZED_URL
          value: "http://oxidized:8888"
        - name: OXIDIZED_PASSWORD_FILE
          value: "/run/secrets/oxidized-password"
      volumeMounts:
        - name: oxidized-password
          mountPath: /run/secrets
          readOnly: true
  volumes:
    - name: oxidized-password
      secret:
        secretName: oxidized-credentials
```

---

## Compatibility

### Oxidized Versions

| Component | Minimum Version | Notes |
|-----------|-----------------|-------|
| Oxidized | 0.35.0+ | Core backup engine |
| oxidized-web | 0.18.0+ | REST API provider |

**Important:** The REST API is provided by [oxidized-web](https://github.com/ytti/oxidized-web), a separate Ruby gem. Ensure both Oxidized and oxidized-web are installed.

### Known API Quirks

mcp-oxidized handles these oxidized-web API quirks automatically:
- `/stats` endpoint returns 404 → computed from `/nodes.json`
- NodeNotFound returns HTTP 500 with Ruby stack trace → parsed and converted
- `/node/show/{name}.json` missing fields → filled with fallback logic
