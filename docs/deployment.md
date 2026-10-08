# Deployment

How to run Arcana as a persistent service and expose it to remote clients.

## Quick setup

```bash
curl -fsSL https://raw.githubusercontent.com/vsbuffalo/arcana/main/install.sh | sh
# or, with arcana already installed:
arcana setup                                   # vault at ~/vault/notes
arcana --vault ~/notes setup --public-host notes.example.com
arcana setup --dry-run                         # show the plan only
```

`arcana setup` is idempotent: each step checks the current state and does
only what is missing. It

1. creates the vault (with word-level attribution enabled) if needed and
   points `~/.config/arcana/config.toml` at it;
2. writes server settings to `~/.config/arcana/server.toml` (mode 0600):
   port, public hostnames, and OAuth secrets (generated, or moved out of an
   existing launchd job so connected clients keep working);
3. installs the background service (launchd on macOS, `systemd --user` on
   Linux); the service definition holds no secrets;
4. registers the MCP server with Claude Code;
5. checks the server answers and prints how to connect claude.ai and ChatGPT.

The rest of this page explains each piece, for manual setups.

## Transports

Arcana's MCP server supports three transports:

| Transport | Flag | Use case |
|-----------|------|----------|
| **stdio** | `--transport stdio` (default) | Claude Code, local MCP clients |
| **SSE** | `--transport sse` | Claude.ai, remote clients, always-on service |

The SSE transport serves both the modern [streamable HTTP](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http) endpoint (`POST /mcp`) and the legacy SSE transport (`GET /sse` + `POST /message`).

## Running as a service

### macOS (launchd)

Create `~/Library/LaunchAgents/com.arcana.serve.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.arcana.serve</string>

    <key>ProgramArguments</key>
    <array>
        <string>/path/to/arcana</string>
        <string>serve</string>
        <string>--transport</string>
        <string>sse</string>
        <string>--port</string>
        <string>8787</string>
        <string>--vault</string>
        <string>/path/to/your/vault</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <true/>

    <key>StandardOutPath</key>
    <string>/Users/you/Library/Logs/arcana-serve.log</string>

    <key>StandardErrorPath</key>
    <string>/Users/you/Library/Logs/arcana-serve.err.log</string>

    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/Users/you/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
        <key>RUST_LOG</key>
        <string>info</string>
        <!-- Auth tokens — see Authentication section below -->
        <key>ARCANA_BEARER_TOKEN</key>
        <string>your-token-here</string>
    </dict>
</dict>
</plist>
```

Load the service:

```bash
launchctl load ~/Library/LaunchAgents/com.arcana.serve.plist
```

After rebuilding arcana, restart the service:

```bash
launchctl kickstart -k gui/$(id -u)/com.arcana.serve
```

Check logs:

```bash
tail -f ~/Library/Logs/arcana-serve.log
tail -f ~/Library/Logs/arcana-serve.err.log
```

### Linux (systemd)

Create `~/.config/systemd/user/arcana-serve.service`:

```ini
[Unit]
Description=Arcana MCP Server
After=network.target

[Service]
ExecStart=/path/to/arcana serve --transport sse --port 8787 --vault /path/to/vault
Restart=always
RestartSec=5
Environment=RUST_LOG=info
Environment=ARCANA_BEARER_TOKEN=your-token-here

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now arcana-serve
journalctl --user -u arcana-serve -f   # tail logs
```

## Authentication

For remote deployments, always enable auth. The server supports two mechanisms that work simultaneously:

### Static bearer token

Simple shared-secret auth. Good for personal use.

```bash
# Generate a token
TOKEN=$(openssl rand -base64 32)

# Pass via flag or env var
arcana serve --bearer-token "$TOKEN" ...
# or
export ARCANA_BEARER_TOKEN="$TOKEN"
```

Clients send `Authorization: Bearer <token>` on every request.

### OAuth 2.1 with PKCE

Full OAuth flow for MCP clients that support it (claude.ai, ChatGPT). Requires three values (`arcana setup` generates them):

```bash
export ARCANA_OAUTH_CLIENT_ID="your-client-id"
export ARCANA_OAUTH_CLIENT_SECRET="your-client-secret"
export ARCANA_OAUTH_PASSWORD="your-password"
```

The OAuth flow:
1. Client discovers endpoints via `GET /.well-known/oauth-authorization-server`
2. Client registers via `POST /register` and receives a public `client_id` bound to its redirect URI (no secret; see below)
3. User authorizes via browser (`GET /authorize`) — enters the password
4. Client exchanges code for access token (`POST /token`)

Clients that register themselves (ChatGPT, and MCP clients generally)
become *public* clients: `/register` issues a client id derived from their
redirect URI, and `/token` accepts it without a secret. Access still needs
the password at `/authorize`, and PKCE binds the code to the client.
Clients given the configured id and secret by hand (e.g. claude.ai's
advanced settings) authenticate with the secret as before.

Both bearer token and OAuth can be active at the same time. Rather than
putting them in the service definition, keep them in
`~/.config/arcana/server.toml` (what `arcana setup` writes); flags and
environment variables override it:

```toml
port = 8787
public_hosts = ["notes.example.com"]
bearer_token = "…"

[oauth]
client_id = "…"
client_secret = "…"
password = "…"
```

Access tokens are held in memory: restarting the server signs connected
clients out, and they ask to authorize again.

## Allowing the public hostname

The server accepts requests only for hostnames it knows. Loopback names
(`localhost`, `127.0.0.1`, `::1`) always work; any name a client reaches it
through must be listed, or every request is refused with
`rejected request with disallowed Host header` in the log. This protects a
server on your machine from DNS rebinding, where a web page tricks your
browser into sending it requests.

```bash
arcana setup --public-host notes.example.com      # stored in server.toml
# or: arcana serve --public-host notes.example.com
# or: ARCANA_PUBLIC_HOSTS=notes.example.com arcana serve
```

This applies to both Tailscale and Cloudflare below.

## Remote access over Tailscale (recommended)

For personal use, the cleanest way to reach Arcana from your other devices is [Tailscale](https://tailscale.com): keep the server bound to loopback and let Tailscale expose it to *your tailnet only* — encrypted end to end, authenticated by device, and invisible to your LAN and the public internet.

With the server running on `127.0.0.1:8787` (the default bind), publish that port to your tailnet with `tailscale serve`:

```bash
tailscale serve --bg 8787
```

Then allow that hostname (see above): `arcana setup --public-host <machine>.<your-tailnet>.ts.net`.

Tailscale proxies `https://<machine>.<your-tailnet>.ts.net/` to the local port (exact flags vary by Tailscale version — see `tailscale serve --help` / status with `tailscale serve status`). Point MCP clients on your other devices at that HTTPS URL.

Because requests reach Arcana over the local proxy (from loopback) and only enrolled tailnet devices can connect, you get device-level authentication at the network layer without exposing the vault. The bearer token / OAuth below is then optional defense-in-depth rather than your only gate. This keeps Arcana's fail-closed default intact: it stays bound to loopback, never to a public interface.

## Public exposure with Cloudflare Tunnel

If you need to reach the server from outside your tailnet, expose it to the internet without opening ports — and **enable authentication first** (see above):

```bash
# Install cloudflared
brew install cloudflared   # macOS
# or see https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/get-started/

# Create a tunnel
cloudflared tunnel create arcana
cloudflared tunnel route dns arcana vault.yourdomain.com

# Run it
cloudflared tunnel run --url http://localhost:8787 arcana
```

Allow the hostname (see above): `arcana setup --public-host vault.yourdomain.com`.
Then connect Claude.ai, ChatGPT or other remote clients to
`https://vault.yourdomain.com/mcp`.

Keep the tunnel running as a service too (`cloudflared service install`), and
if Cloudflare's bot protection is on for the zone, make sure it does not block
requests from OpenAI's and Anthropic's servers.

## Connecting clients

### Claude Code (stdio — recommended for local)

Add to `.mcp.json` in your project:

```json
{
  "mcpServers": {
    "arcana": {
      "command": "arcana",
      "args": ["serve", "--vault", "/path/to/your/vault"]
    }
  }
}
```

No network, no auth — runs as your user.

### Claude Code (remote SSE)

```bash
claude mcp add --transport sse arcana https://your-server.example.com/sse \
  --header "Authorization: Bearer YOUR_TOKEN"
```

### Claude.ai

Settings → Connectors → Add custom connector, with
`https://<public host>/mcp`. Claude.ai runs the OAuth flow; authorize with the
password from `server.toml`.

### ChatGPT

**Use the ChatGPT desktop app.** As of October 2026, adding a custom MCP
server worked in the desktop app (Plugins → MCPs → Add → Connect to a custom
MCP), while ChatGPT on the web offered only plugins from its directory for
the account we tested. The reason is not documented; restricting the web to
reviewed plugins for security is a likely one. Availability also depends on
the plan: OpenAI documents full MCP, including writes, for Business,
Enterprise and Edu, and read/fetch tools for Pro
([OpenAI Help](https://help.openai.com/en/articles/12584461-developer-mode-and-mcp-apps-in-chatgpt)).
Arcana marks `vault_search`, `vault_read` and `vault_decisions` read-only, so
those work either way.

The desktop form has two types. `arcana setup` prints the values for both.

**STDIO** (the app starts arcana on the same machine; no tunnel or password):

```
Name: arcana
Type: STDIO
Command to launch: /Users/you/.cargo/bin/arcana
Arguments (one per row):
  --vault
  /Users/you/vault/notes
  serve
Environment variables: (none)
Working directory: (leave empty)
```

**Streamable HTTP** (through your public hostname; works from any machine):

```
Name: arcana
Type: Streamable HTTP
URL: https://<public host>/mcp
Authentication: OAuth
```

ChatGPT registers itself and asks you to authorize with the password from
`~/.config/arcana/server.toml`.

## File watcher

When running as a service (`--transport sse`), arcana watches the vault for file changes:

- **Reindex** — immediately on file change (search stays fresh)
- **Git commit** — batched periodically (default: every 5 minutes)

The commit interval is configurable:

```toml
# ~/.config/arcana/config.toml
[git]
commit_interval_secs = 300   # default: 5 minutes, 0 = commit on every change
```

Human edits (from Obsidian, vim, etc.) are committed with your git identity. AI-generated content approved via `arcana review` uses the AI identity (`arcana-ai <ai@arcana.local>`). This makes `git blame` and `git log --author` useful for distinguishing authorship.
