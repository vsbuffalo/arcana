# Arcana — Quick Tour

A hands-on walkthrough. Takes about 2 minutes.

---

## Install it

```bash
cargo install --path crates/arcana-cli
```

This puts an `arcana` binary in your `~/.cargo/bin/`. First install takes ~15s (compiles SQLite from source). Done once.

## Use it on your vault

`cd` into your Obsidian vault and go:

```bash
cd ~/path/to/your/obsidian/vault

arcana index
```

```
Index complete
  Scanned: 347
  Added: 347
  Updated: 0
  Removed: 0
  Unchanged: 0
```

It walks every `.md` file, parses frontmatter + wikilinks + tags in parallel, and builds a SQLite FTS5 index. The DB goes in `.arcana/index.db` inside your vault (already gitignored).

Run it again — it's incremental. Only changed files get re-indexed:

```bash
arcana index
```

```
Index complete
  Scanned: 347
  Added: 0
  Updated: 0
  Removed: 0
  Unchanged: 347
```

## Search

```bash
arcana search "machine learning"
arcana search "rust" --tag programming
arcana search "todo" --path daily/
arcana search "attention mechanism" --limit 5
```

Full-text search with BM25 ranking and highlighted snippets. Title matches rank higher than body matches.

## Read a note

```bash
arcana read "research/some-note.md"
```

Shows title, tags, and the full markdown body.

## Stats

```bash
arcana stats
```

```
Vault Statistics
  Notes: 347
  Unique tags: 89
  Wikilinks: 412
```

## Create a note

```bash
arcana create "ideas/new-idea.md" --title "New Idea" --tags "idea,brainstorm" --body "Some initial thoughts"
```

Creates the file with YAML frontmatter and immediately indexes it.

## JSON output

Every command supports `--json`:

```bash
arcana search "rust" --json
arcana stats --json
arcana read "some/note.md" --json
```

## How it finds your vault

In order of priority:

1. `--vault /explicit/path`
2. `ARCANA_VAULT` env var
3. Walk up from `cwd` looking for `.obsidian/` (this is why `cd`-ing into your vault just works)

## MCP Server (for AI assistants)

Arcana includes an MCP server so Claude Code, Claude Web, and other MCP clients can search, read, and write notes in your vault.

### Claude Code

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

Claude Code will automatically start the server over stdio. You can then ask Claude to search your vault, read notes, create new ones, etc.

### Claude Web / remote clients

Start the HTTP server:

```bash
arcana serve --vault ~/my-vault --transport sse --port 8080
```

Then configure your MCP client to connect to `http://localhost:8080/mcp`.

### Available tools

| Tool           | Description                                       |
| -------------- | ------------------------------------------------- |
| `vault_search` | Full-text search with filters (tags, path prefix) |
| `vault_read`   | Read a note's full content                        |
| `vault_create` | Create a new note with title, tags, body          |
| `vault_update` | Update body, append text, add/remove tags         |
| `vault_list`   | List notes with optional filters                  |
| `vault_stats`  | Get vault statistics (notes, tags, links)         |

## For development

```bash
cargo test --workspace                              # 245 tests
cargo clippy --workspace --all-targets -- -D warnings  # zero warnings
```

There's a fixture vault at `tests/fixtures/small_vault/` with 10 notes covering frontmatter, wikilinks, inline tags, nested folders, custom fields, and edge cases.

## Source layout

```
crates/arcana-core/src/
├── errors.rs       # error types
├── note.rs         # Note, Frontmatter, wikilink/tag extraction
├── config.rs       # TOML config, DB path resolution
├── index/
│   ├── schema.rs   # SQLite DDL, FTS5, triggers, WAL mode
│   └── fts.rs      # upsert/delete/query helpers
├── vault.rs        # central coordinator — owns DB, does index/search/write
├── search.rs       # FTS5 search + BM25 ranking + query sanitizer
├── writer.rs       # atomic writes (tmpfile → fsync → rename)
└── watcher.rs      # filesystem watcher (notify + debouncer, OS thread)

crates/arcana-cli/src/
├── main.rs         # clap, vault auto-detection
├── output.rs       # colored terminal formatting
└── commands/       # one module per subcommand (search, read, create, blame, ingest, review, ledger, …)
```
