# Arcana

A single-user, self-hosted knowledge store that turns your own notes into live context for AI.

Arcana indexes a folder of markdown notes — your research, references, project docs, ideas, the context you've accumulated over years — and exposes them to AI assistants via [MCP](https://modelcontextprotocol.io) (Model Context Protocol). Claude, and any MCP-compatible client, can search, read, and write notes in your vault mid-conversation, without copy-pasting or uploading files.

It's built for one person and one vault: a personal context store you keep, curate, and reuse — running on your own machine, not someone else's cloud. It works especially well with [Obsidian](https://obsidian.md) vaults (respects frontmatter and wikilinks), but any directory of markdown files works.

## Why

LLMs are powerful but context-starved. You already have a personal knowledge base — years of notes, bookmarks, clipped papers, research threads. Arcana bridges the gap: your notes become live context that AI assistants can pull from mid-conversation, without copy-pasting or uploading files.

- **Search** — full-text search with BM25 ranking, tag/path filters, highlighted snippets
- **Read** — pull any note's content into the conversation
- **Write** — AI drafts new notes that land in your vault, properly formatted with frontmatter
- **Local-first & private** — your notes live on your machine; the server binds to loopback by default and fail-closes rather than exposing your vault unauthenticated

## Self-hosted, single-user, private

Arcana is deliberately **single-user**: one owner, one vault, one set of credentials. No multi-tenancy, no per-user isolation, no accounts to manage. That constraint keeps the whole system simple and lets it run as a trustworthy, long-lived service on hardware you own.

Because it's meant to run as an always-on service on your own machine, it's built to be **robust**:

- Atomic writes (tmpfile → fsync → rename) — no half-written notes, no silent data loss
- ACID via SQLite WAL; incremental indexing keyed on content hashes
- Git-tracked provenance — every change is committed, with human vs AI authorship distinguishable per line

…and **private by default**:

- The HTTP server binds to loopback (`127.0.0.1`) and *fail-closes* — it refuses to bind a non-loopback address unless you've configured authentication.
- Secrets are compared in constant time, the OAuth bridge enforces PKCE, and request bodies and session counts are bounded.

### Reaching it from your other devices

You don't need to put Arcana on your LAN or the public internet to use it from your phone or laptop. Keep it bound to loopback and reach it over **[Tailscale](https://tailscale.com)** — a private, encrypted, device-authenticated mesh. `tailscale serve` proxies the local port to *your tailnet only*, so the vault stays invisible to everything outside your own devices and Tailscale handles authentication for you. This is the recommended remote-access setup.

If you need genuine public exposure, enable authentication (bearer token or OAuth) and front it with a tunnel. See **[docs/deployment.md](docs/deployment.md)**.

## Install

```bash
cargo install --path crates/arcana-cli
```

This puts an `arcana` binary in `~/.cargo/bin/`. First install takes ~15s (compiles SQLite from source).

## Quick start

Configure your vault path:

```bash
mkdir -p ~/.config/arcana
echo '[vault]
path = "/path/to/your/notes"' > ~/.config/arcana/config.toml
```

Then index:

```bash
arcana index
```

Indexing is incremental — subsequent runs only process changed files. The index lives in `.arcana/index.db` (SQLite, gitignored by default).

### Search

```bash
arcana search "machine learning"
arcana search "rust" --tag programming
arcana search "todo" --path daily/ --limit 5
```

### Read and create notes

```bash
arcana read "research/attention.md"
arcana create "ideas/new-idea.md" --title "New Idea" --tags "idea" --body "Some thoughts"
```

### Machine-readable output

Every command supports `--json` for structured output. When piped (non-TTY), output defaults to plain paths for composability with `grep`, `xargs`, `fzf`, etc.

## MCP server

Arcana ships an MCP server so AI assistants can interact with your vault directly.

### Claude Code (local, stdio)

Add to your project's `.mcp.json`:

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

No network, no auth — runs locally as your user.

### Claude.ai and other MCP clients (HTTP)

Start the HTTP server — it binds to `127.0.0.1` by default:

```bash
arcana serve --vault ~/my-vault --transport sse --port 8787
```

Connect a local client to `http://localhost:8787/mcp`. The server supports both [streamable HTTP](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http) (`POST /mcp`) and legacy SSE (`GET /sse` + `POST /message`).

To use it from another device, keep it on loopback and put it on your tailnet with Tailscale (recommended), or enable authentication and bind a wider address (`--host`/`ARCANA_HOST`). For persistent background service, authentication, and remote access, see **[docs/deployment.md](docs/deployment.md)**.

### MCP tools

| Tool | Description |
|------|-------------|
| `vault_search` | Full-text search with tag and path filters |
| `vault_read` | Read a note's full content |
| `vault_create` | Create a new note with title, tags, body |
| `vault_update` | Update body, append text, add/remove tags |
| `vault_list` | List notes with optional filters |
| `vault_stats` | Vault statistics (notes, tags, links) |

## AI pipelines

Beyond serving notes to AI assistants, Arcana includes agentic pipelines for knowledge extraction. All pipelines follow a **plan-first, human-in-the-loop** workflow — AI never writes directly to your vault.

### Ingest

Reads an external codebase and writes new vault notes from scratch:

```bash
arcana ingest /path/to/project --skill model-extract
```

### Tidy

Reorganizes messy vault notes — moves, splits, extracts concepts:

```bash
arcana tidy notes/inbox/
arcana tidy --audit         # lightweight vault-wide structure check
```

### Review

Approve, reject, or edit AI drafts before they enter the vault:

```bash
arcana review
arcana review --verify-style   # also check against your style guide
```

For detailed pipeline docs, prompt overrides, and provenance tracking, see **[docs/ai-pipelines.md](docs/ai-pipelines.md)**.

## Configuration

```toml
# ~/.config/arcana/config.toml
[vault]
path = "/path/to/your/notes"

[profiles.sonnet]
provider = "anthropic"
model = "claude-sonnet-4-5-20250929"
```

Config merges in layers: compiled defaults → global config → vault-local config → CLI flags.

For the full config reference (LLM profiles, agent settings, git provenance, brain profile), see **[docs/configuration.md](docs/configuration.md)**.

## Architecture

```
arcana-core       library — indexing, search, notes, drafts, git provenance
arcana-agent      LLM integration — ingest, tidy, chat pipelines
arcana-server     MCP server (stdio, streamable HTTP, legacy SSE)
arcana-cli        clap CLI, owns all user-facing output
```

- SQLite FTS5 for full-text search with BM25 ranking
- xxhash for incremental change detection
- rayon for parallel markdown parsing
- Atomic writes (tmpfile, fsync, rename) — no silent data loss
- Git provenance tracking (human vs AI authorship per line via `git blame`)

See [ARCHITECTURE.md](ARCHITECTURE.md) for details.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

There's a fixture vault at `tests/fixtures/small_vault/` with test notes covering frontmatter, wikilinks, tags, nested folders, and edge cases.

## License

MIT
