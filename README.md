# Arcana

Your personal knowledge base as context for AI.

Arcana indexes a folder of markdown notes — your research, references, project docs, ideas — and exposes them to AI assistants via [MCP](https://modelcontextprotocol.io) (Model Context Protocol). Claude, and any MCP-compatible client, can search, read, and write notes in your vault as naturally as browsing the web.

It works especially well with [Obsidian](https://obsidian.md) vaults (respects frontmatter and wikilinks), but any directory of markdown files works.

## Why

LLMs are powerful but context-starved. You already have a personal knowledge base — years of notes, bookmarks, clipped papers, research threads. Arcana bridges the gap: your notes become live context that AI assistants can pull from mid-conversation, without copy-pasting or uploading files.

- **Search** — full-text search with BM25 ranking, tag/path filters, highlighted snippets
- **Read** — pull any note's content into the conversation
- **Write** — AI drafts new notes that land in your vault, properly formatted with frontmatter
- **Local-first** — your notes never leave your machine (unless you choose to expose the server remotely)

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

Start the HTTP server:

```bash
arcana serve --vault ~/my-vault --transport sse --port 8080
```

Connect your client to `http://localhost:8080/mcp`. The server supports both [streamable HTTP](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http) (`POST /mcp`) and legacy SSE (`GET /sse` + `POST /message`).

For persistent background service, authentication, and remote access, see **[docs/deployment.md](docs/deployment.md)**.

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
