# Arcana

[![CI](https://github.com/vsbuffalo/arcana/actions/workflows/ci.yml/badge.svg)](https://github.com/vsbuffalo/arcana/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Arcana is a single-user, self-hosted knowledge store that turns your markdown
notes directory into personal, reusable context for AI.

The central problems I had were:

 1. **One vault, every assistant** — I wanted to be able to pull my Obsidian
    notes into context across Claude web, Claude Code, ChatGPT, etc.
 2. **Provenance for free** — I wanted to enable Git to automatically handle
    AI-vs-human provenance.
 3. **My machine, not the cloud** — I wanted it to run locally on my home
    machine (behind Tailscale), rather than someone else's cloud.
 4. **My context, managed by me** — I wanted to own and curate my context
    myself: I decide what goes in and how it's organized, rather than relying
    on an opaque, automatic memory managed externally.

Why? While using Anthropic's Claude to teach myself electronics, I needed a
quick way to manage my two kinds of notes — per-project and learning-topic. I
also wanted a lab notebook I could talk to over speech-to-text in Claude Code,
so I could say "I measure a voltage across capacitor C3 of 9.4V, can you record
that in my lab notebook?" or ask "how many 2k resistors do I have on hand?". I
wanted it also to store ideas I wrote by hand, all in the same indexed store
for context: I could then ask, "I wanted to understand Wien Bridge Oscillators
better, do we have all the parts on hand to build one?" and Claude could access
my parts inventory and connect it to other ideas I had.

## How it works

Arcana indexes a directory of markdown files and exposes them to any
[MCP](https://modelcontextprotocol.io) client — Claude, Claude Code, and others
— which can search, read, and write notes mid-conversation. It works especially
well with [Obsidian](https://obsidian.md) vaults (frontmatter, wikilinks), but
any folder of markdown files works.

- **Search** — SQLite **FTS5** full-text with BM25 ranking, tag/path filters,
  highlighted snippets; xxhash incremental indexing, rayon-parallel parsing
- **MCP server** — one server, three transports (stdio, streamable HTTP, legacy
  SSE); tools `vault_search` / `read` / `create` / `update` / `list` / `stats`
- **Provenance** — every change is git-committed and AI writes use a distinct
  identity, so `git blame` attributes each line human-vs-AI (`arcana blame`)
- **Durable** — note writes are atomic and crash-safe (temp → fsync → rename →
  fsync parent dir); the index is ACID via SQLite WAL. Readers never see a
  half-written note, and a completed write survives a crash
- **Private by default** — binds to loopback and fail-closes without auth;
  constant-time secret checks, PKCE, bounded request bodies and sessions
- **AI pipelines** — `ingest` / `review`, all plan-first and
  human-in-the-loop; AI output lands in drafts, never the vault unreviewed

Four Rust crates with clean seams:

```
arcana-core     index, search, notes, drafts, git provenance (zero UI deps)
arcana-agent    LLM pipelines — ingest, chat
arcana-server   MCP server — stdio, streamable HTTP, legacy SSE
arcana-cli      clap CLI — owns all user-facing output
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the system design, and
[docs/retrieval.md](docs/retrieval.md) for how search and indexing work.

## Status & limitations

Alpha, and deliberately single-user — it runs as a personal service on my own
machine, so the design assumes one trusted user rather than multi-tenancy. The
known rough edges I haven't closed yet: the
`ingest` and `chat` turn loops are duplicated; the filesystem watcher thread
isn't joined on shutdown; and `LIKE` path-prefix filters don't yet escape
`%`/`_`. Security hardening (path-traversal, OAuth/PKCE, constant-time secret
checks, DoS bounds) is done — see the git history.

## Install

```bash
cargo install --path crates/arcana-cli
```

Puts an `arcana` binary in `~/.cargo/bin/` (first build ~15s — compiles SQLite).

## Quick start

```bash
mkdir -p ~/.config/arcana
echo '[vault]
path = "/path/to/your/notes"' > ~/.config/arcana/config.toml

arcana index                                     # incremental; index in .arcana/index.db
arcana search "voltage regulator" --tag electronics
arcana read   "research/attention.md"
arcana create "ideas/new.md" --title "New" --tags idea --body "..."
arcana blame  "research/attention.md"            # per-line human-vs-AI provenance
```

`--json` on any command gives structured output; piped (non-TTY) output is plain
paths, for `grep` / `xargs` / `fzf`.

For a hands-on walkthrough with example output, see [docs/tour.md](docs/tour.md).

## MCP server

**Local — Claude Code (stdio).** Add to `.mcp.json`:

```json
{
  "mcpServers": {
    "arcana": { "command": "arcana", "args": ["serve", "--vault", "/path/to/vault"] }
  }
}
```

**HTTP — Claude.ai and remote clients.** Binds `127.0.0.1` by default:

```bash
arcana serve --vault ~/my-vault --transport sse --port 8787
```

Serves streamable HTTP (`POST /mcp`) and legacy SSE (`GET /sse` + `POST /message`).
For remote access over Tailscale, authentication, and running as a service, see
**[docs/deployment.md](docs/deployment.md)**.

## AI pipelines

Plan-first and human-in-the-loop — AI never writes directly to your vault:

```bash
arcana ingest /path/to/project --skill model-extract   # codebase → new notes
arcana review                                           # approve/reject AI drafts
```

See **[docs/ai-pipelines.md](docs/ai-pipelines.md)**.

## Configuration

```toml
# ~/.config/arcana/config.toml
[vault]
path = "/path/to/your/notes"

[profiles.sonnet]
provider = "anthropic"
model = "claude-sonnet-4-5-20250929"
```

Layered: compiled defaults → global → vault-local → CLI flags. Full reference in
**[docs/configuration.md](docs/configuration.md)**.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Test fixture vault at `tests/fixtures/small_vault/`.

## License

MIT
