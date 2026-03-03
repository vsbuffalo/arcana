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

## For development

```bash
cargo test --workspace                              # 40 tests
cargo clippy --workspace --all-targets -- -D warnings  # zero warnings
```

There's a fixture vault at `tests/fixtures/small_vault/` with 9 notes covering frontmatter, wikilinks, inline tags, nested folders, custom fields, and edge cases.

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
└── commands/       # index, search, read, create, stats
```
