# Obsidian MCP Server — Engineering Plan

> **Project codename:** `vault-mcp`
> **Language:** Rust (2021 edition, MSRV 1.75+)
> **License:** MIT
> **Goal:** The fastest, most correct local-first MCP server for Obsidian vaults with an embedded AI research agent.

---

## 1. Project Philosophy

This is an engineering showcase. Every design decision optimizes for:

1. **Speed** — Sub-10ms search on 10k+ note vaults. Measured, not assumed.
2. **Correctness** — No silent data loss. Crash-safe writes. Proper ACID via SQLite WAL mode.
3. **Observability** — Built-in profiling from day one. If you can't measure it, you can't optimize it.
4. **Composability** — CLI, MCP server, TUI, and agent are independent layers over a shared core library.
5. **Local-first** — Everything works offline except LLM API calls. Your vault never leaves your machine.

---

## 2. Architecture

### 2.1 Layer Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                        Interface Layer                           │
│  ┌──────────┐  ┌──────────────┐  ┌───────┐  ┌───────────────┐  │
│  │   CLI    │  │  MCP Server  │  │  TUI  │  │  HTTP API     │  │
│  │  (clap)  │  │ (stdio/SSE)  │  │(ratatui│  │  (future)     │  │
│  └────┬─────┘  └──────┬───────┘  └───┬───┘  └───────┬───────┘  │
│       │               │              │               │           │
│       └───────────────┼──────────────┼───────────────┘           │
│                       │              │                           │
│  ┌────────────────────▼──────────────▼──────────────────────┐   │
│  │                    Core Library (vault-core)               │   │
│  │                                                            │   │
│  │  ┌──────────┐ ┌───────────┐ ┌──────────┐ ┌────────────┐  │   │
│  │  │ Indexer  │ │  Searcher │ │  Writer  │ │  Watcher   │  │   │
│  │  │          │ │           │ │          │ │  (notify)  │  │   │
│  │  │ • parse  │ │ • fts5    │ │ • create │ │            │  │   │
│  │  │ • front- │ │ • vector  │ │ • update │ │ • debounce │  │   │
│  │  │   matter │ │ • hybrid  │ │ • front- │ │ • incr.    │  │   │
│  │  │ • embed  │ │ • RRF     │ │   matter │ │   re-index │  │   │
│  │  └────┬─────┘ └─────┬─────┘ └────┬─────┘ └─────┬──────┘  │   │
│  │       │             │            │              │          │   │
│  │  ┌────▼─────────────▼────────────▼──────────────▼──────┐  │   │
│  │  │              SQLite (WAL mode)                       │  │   │
│  │  │  ┌─────────┐  ┌──────────┐  ┌───────────────────┐   │  │   │
│  │  │  │  FTS5   │  │ metadata │  │  vec0 (sqlite-vec) │   │  │   │
│  │  │  │  index  │  │  tables  │  │  embeddings        │   │  │   │
│  │  │  └─────────┘  └──────────┘  └───────────────────┘   │  │   │
│  │  └──────────────────────────────────────────────────────┘  │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │                    Agent Layer                              │   │
│  │  ┌──────────────┐  ┌─────────────┐  ┌──────────────────┐  │   │
│  │  │  Agent Loop  │  │ LLM Backend │  │  Prompt Library  │  │   │
│  │  │  (tool-use   │  │  (trait)     │  │                  │  │   │
│  │  │   cycle)     │  │  • Anthropic │  │  • research      │  │   │
│  │  │              │  │  • OpenAI    │  │  • summarize     │  │   │
│  │  │  max_iter:20 │  │  • Ollama    │  │  • organize      │  │   │
│  │  └──────────────┘  └─────────────┘  └──────────────────┘  │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │                 Observability Layer                         │   │
│  │  tracing (spans) + metrics (histograms) + profiling hooks  │   │
│  └────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Crate / Workspace Layout

This is a Cargo workspace. Separation allows the core library to be tested independently and reused across interfaces.

```
vault-mcp/
├── Cargo.toml                    # Workspace root
├── config.example.toml
├── README.md
├── ARCHITECTURE.md               # This document, kept in-repo
│
├── crates/
│   ├── vault-core/               # The library. Zero UI dependencies.
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs         # Typed config (serde + toml)
│   │       ├── note.rs           # Note struct, frontmatter serde
│   │       ├── vault.rs          # Vault struct: open, scan, stats
│   │       ├── index/
│   │       │   ├── mod.rs
│   │       │   ├── schema.rs     # SQLite schema migrations
│   │       │   ├── fts.rs        # FTS5 indexing + querying
│   │       │   ├── vector.rs     # sqlite-vec embedding storage + query
│   │       │   ├── hybrid.rs     # RRF merge of fts + vector results
│   │       │   └── profiling.rs  # Index-level perf instrumentation
│   │       ├── search.rs         # Public search API (combines index/)
│   │       ├── writer.rs         # Create/update notes with frontmatter
│   │       ├── watcher.rs        # Filesystem watcher (notify), debounced
│   │       ├── embeddings/
│   │       │   ├── mod.rs
│   │       │   ├── traits.rs     # EmbeddingBackend trait
│   │       │   ├── anthropic.rs  # Voyage (via Anthropic)
│   │       │   ├── openai.rs     # text-embedding-3-small
│   │       │   └── ollama.rs     # Local (nomic-embed-text, etc.)
│   │       └── errors.rs         # thiserror error types
│   │
│   ├── vault-mcp-server/         # MCP protocol server
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── server.rs         # MCP JSON-RPC handler
│   │       ├── tools.rs          # Tool definitions + dispatch
│   │       ├── resources.rs      # MCP resources (note:// URIs)
│   │       └── transport/
│   │           ├── mod.rs
│   │           ├── stdio.rs      # For Claude Desktop
│   │           └── sse.rs        # For Claude web / remote
│   │
│   ├── vault-agent/              # AI agent (research, summarize, etc.)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── agent_loop.rs     # Core tool-use cycle
│   │       ├── prompts.rs        # System prompts, per-task
│   │       ├── tasks/
│   │       │   ├── mod.rs
│   │       │   ├── research.rs   # "Research X and create notes"
│   │       │   ├── summarize.rs  # "Summarize these notes"
│   │       │   └── organize.rs   # "Re-tag / re-link notes"
│   │       └── llm/
│   │           ├── mod.rs
│   │           ├── traits.rs     # LlmBackend trait
│   │           ├── anthropic.rs
│   │           ├── openai.rs
│   │           └── ollama.rs
│   │
│   ├── vault-cli/                # CLI binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs           # clap App definition
│   │       ├── commands/
│   │       │   ├── mod.rs
│   │       │   ├── search.rs
│   │       │   ├── create.rs
│   │       │   ├── index.rs      # Force re-index, stats
│   │       │   ├── research.rs   # Invoke agent
│   │       │   ├── serve.rs      # Start MCP server
│   │       │   └── profile.rs    # Run profiling benchmarks
│   │       └── output.rs         # Pretty terminal output (colored)
│   │
│   └── vault-tui/                # TUI (phase 2)
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── app.rs            # ratatui App state
│           ├── ui/
│           │   ├── mod.rs
│           │   ├── search_view.rs
│           │   ├── note_view.rs
│           │   ├── agent_view.rs # Live agent progress
│           │   └── profile_view.rs
│           └── events.rs         # Input handling
│
├── benches/                      # Criterion benchmarks
│   ├── indexing.rs
│   ├── search_fts.rs
│   ├── search_vector.rs
│   └── search_hybrid.rs
│
├── tests/                        # Integration tests
│   ├── fixtures/                 # Sample vaults for testing
│   │   ├── small_vault/          # 10 notes
│   │   ├── medium_vault/         # 500 notes (generated)
│   │   └── large_vault/          # 10k notes (generated)
│   ├── test_indexing.rs
│   ├── test_search.rs
│   ├── test_mcp.rs
│   └── test_agent.rs
│
└── scripts/
    ├── generate_test_vault.py    # Generate synthetic vaults for benchmarks
    └── profile_report.sh         # Run benchmarks, generate flamegraph
```

---

## 3. Core Library (`vault-core`) — Detailed Design

### 3.1 Note Struct & Frontmatter

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    /// Path relative to vault root
    pub path: PathBuf,
    pub frontmatter: Frontmatter,
    /// Raw markdown body (excluding frontmatter)
    pub body: String,
    /// File-level metadata
    pub file_meta: FileMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub modified: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    /// AI generation metadata — None for human-authored notes
    pub ai: Option<AiMeta>,
    /// Catch-all for user-defined fields we don't know about
    #[serde(flatten)]
    pub extra: serde_yaml::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiMeta {
    pub model: String,
    pub provider: String,           // "anthropic", "openai", "ollama"
    pub agent_session: String,      // UUID linking related notes from one research run
    pub task: String,               // "research", "summarize", "organize"
    pub prompt: String,             // The original user prompt that triggered this
    pub sources: Vec<String>,       // URLs or vault:// paths used as context
    pub confidence: Confidence,
    pub reviewed: bool,             // Human has reviewed this — defaults false
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
    Speculative,
}

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size_bytes: u64,
    pub modified_on_disk: std::time::SystemTime,
    pub content_hash: u64, // xxhash for fast change detection
}
```

**Frontmatter parsing strategy:** Split on the first `---\n...\n---` boundary. Parse YAML with `serde_yaml`. The `#[serde(flatten)] extra` field preserves any user-defined frontmatter we don't model — this is critical so we never destroy user data on round-trip.

**Content hashing:** Use `xxhash` (via `xxhash-rust` crate) instead of SHA-256. We're not doing cryptography; we're detecting file changes. xxhash is ~10x faster and perfectly adequate for this use case.

### 3.2 SQLite Schema

Run all migrations on first open. Use a `schema_version` pragma to track versions.

```sql
-- Enable WAL mode for concurrent read/write
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

-- Core notes table
CREATE TABLE IF NOT EXISTS notes (
    id            INTEGER PRIMARY KEY,
    path          TEXT NOT NULL UNIQUE,        -- relative to vault root
    title         TEXT,
    content_hash  INTEGER NOT NULL,            -- xxhash of full file content
    frontmatter   TEXT,                        -- raw YAML (for reconstruction)
    body          TEXT NOT NULL,               -- markdown body without frontmatter
    created_at    TEXT,                        -- from frontmatter or file mtime
    modified_at   TEXT,
    is_ai         BOOLEAN NOT NULL DEFAULT 0,
    ai_model      TEXT,
    ai_session    TEXT,
    ai_reviewed   BOOLEAN DEFAULT 0,
    indexed_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- FTS5 full-text index
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    title,
    body,
    tags,
    content = 'notes',
    content_rowid = 'id',
    tokenize = 'porter unicode61 remove_diacritics 2'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, title, body, tags)
    VALUES (new.id, new.title, new.body, '');
END;

CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, body, tags)
    VALUES ('delete', old.id, old.title, old.body, '');
END;

CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, body, tags)
    VALUES ('delete', old.id, old.title, old.body, '');
    INSERT INTO notes_fts(rowid, title, body, tags)
    VALUES (new.id, new.title, new.body, '');
END;

-- Tags (normalized, for fast filtering)
CREATE TABLE IF NOT EXISTS tags (
    note_id  INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    PRIMARY KEY (note_id, tag)
);
CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);

-- Wikilinks (for graph traversal)
CREATE TABLE IF NOT EXISTS links (
    source_id  INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    target     TEXT NOT NULL,     -- target path or alias (may not exist yet)
    PRIMARY KEY (source_id, target)
);
CREATE INDEX IF NOT EXISTS idx_links_target ON links(target);

-- Vector embeddings (sqlite-vec)
-- This is loaded as an extension; the table creation uses vec0
CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
    note_id INTEGER PRIMARY KEY,
    embedding float[1536]          -- dimension depends on model; 1536 for OpenAI
);

-- Profiling / stats
CREATE TABLE IF NOT EXISTS index_stats (
    run_id        TEXT PRIMARY KEY,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    notes_scanned INTEGER,
    notes_updated INTEGER,
    notes_added   INTEGER,
    notes_removed INTEGER,
    duration_ms   INTEGER,
    errors        TEXT              -- JSON array of error messages
);
```

**Key decisions:**

- **WAL mode** allows concurrent reads during writes. The file watcher can trigger re-indexing without blocking search queries.
- **Content hash in DB** means we can skip re-parsing notes that haven't changed. On a 10k vault, this reduces re-index from ~5s to <100ms for incremental updates.
- **FTS5 `porter unicode61`** tokenizer gives us stemming ("running" matches "run") and Unicode support. The `remove_diacritics 2` handles accented characters.
- **Embedding dimension is configurable.** The schema above uses 1536 (OpenAI default). For Voyage or local models, adjust. Store the dimension in a config/meta table so we error early if the model changes.

### 3.3 Indexing Pipeline

```
Full Index Flow:
================

walkdir(vault_root)
    │
    ├── Filter: only .md files
    ├── Filter: skip .obsidian/, .trash/, hidden files
    │
    ▼
For each .md file (parallelized with rayon):
    │
    ├── Read file bytes
    ├── Compute xxhash
    ├── Check DB: does content_hash match?
    │   ├── YES → skip (already indexed)
    │   └── NO  → continue
    ├── Parse frontmatter (serde_yaml)
    ├── Extract body markdown
    ├── Extract wikilinks ([[target]] and [[target|alias]])
    ├── Extract tags (#tag and frontmatter tags)
    ├── [SPAN: "parse_note"] record duration
    │
    └── Yield IndexEntry {
            path, title, body, frontmatter,
            content_hash, tags, links, ai_meta
        }

Batch insert into SQLite (in a transaction):
    │
    ├── DELETE notes WHERE path NOT IN scanned_paths  (removed files)
    ├── UPSERT notes (INSERT OR REPLACE)
    ├── FTS5 triggers fire automatically
    ├── UPSERT tags
    ├── UPSERT links
    ├── [SPAN: "db_write"] record duration
    │
    └── Commit transaction

Background (async, after index commit):
    │
    ├── For notes with no embedding or stale embedding:
    │   ├── Batch embed (chunked, up to 100 at a time for API)
    │   ├── UPSERT into note_embeddings
    │   └── [SPAN: "embed_batch"] record duration
    │
    └── Record index_stats row
```

**Performance targets:**

| Vault Size   | Full Index | Incremental (1 file changed) |
| ------------ | ---------- | ---------------------------- |
| 100 notes    | < 200ms    | < 20ms                       |
| 1,000 notes  | < 1s       | < 20ms                       |
| 10,000 notes | < 5s       | < 50ms                       |

**Implementation notes:**

- Use `rayon` for parallel file reading + parsing. SQLite writes must be serial (single writer), but parsing is the bottleneck anyway.
- Batch SQLite inserts in chunks of 500 within a single transaction. This is ~100x faster than individual inserts.
- Embeddings happen asynchronously after the text index is ready. Search works immediately with FTS5-only; vector results appear once embeddings complete.
- The file watcher (`notify` crate) uses a **debounce window of 500ms** to coalesce rapid saves (Obsidian auto-saves frequently).

### 3.4 Search — Hybrid FTS5 + Vector with RRF

```rust
/// Public search API. This is what CLI, MCP, TUI, and Agent all call.
pub struct SearchEngine {
    db: Arc<rusqlite::Connection>,  // Behind a connection pool in practice
    embedder: Option<Arc<dyn EmbeddingBackend>>,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub limit: usize,              // default 20
    pub mode: SearchMode,
    pub filters: SearchFilters,
}

#[derive(Debug, Clone)]
pub enum SearchMode {
    Fts,         // FTS5 only (fastest, keyword-match)
    Vector,      // Vector only (semantic, requires embeddings)
    Hybrid,      // Both, merged with RRF (best quality)
    Auto,        // Hybrid if embeddings available, else FTS
}

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub tags: Vec<String>,         // AND filter: note must have ALL these tags
    pub exclude_tags: Vec<String>,
    pub path_prefix: Option<String>,
    pub ai_only: Option<bool>,
    pub unreviewed_only: bool,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub note_id: i64,
    pub path: PathBuf,
    pub title: Option<String>,
    pub snippet: String,           // Highlighted snippet from FTS5 or body
    pub score: f64,                // Normalized 0..1
    pub match_source: MatchSource, // Which index matched
}

#[derive(Debug, Clone)]
pub enum MatchSource {
    Fts { rank: f64 },
    Vector { similarity: f64 },
    Hybrid { fts_rank: f64, vector_sim: f64, rrf_score: f64 },
}
```

**Reciprocal Rank Fusion (RRF) algorithm:**

```
Given:
  fts_results:    [(note_id, rank_position), ...]  sorted by FTS5 rank
  vector_results: [(note_id, rank_position), ...]  sorted by cosine similarity

For each unique note_id across both result sets:
  rrf_score = 0
  if note_id in fts_results at position i:
      rrf_score += 1 / (k + i)        // k = 60 (standard constant)
  if note_id in vector_results at position j:
      rrf_score += 1 / (k + j)

Sort by rrf_score descending. Return top N.
```

RRF is simple, parameter-free (k=60 is standard), and produces excellent results. It naturally handles the case where one index returns something the other doesn't — that result just gets a lower score.

**Non-blocking search guarantee:**

- Search uses a **read-only** SQLite connection (separate from the write connection used by the indexer).
- WAL mode allows unlimited concurrent readers.
- The search function is sync but fast (< 10ms target). It never blocks on indexing.
- For the async interfaces (MCP server, TUI), wrap in `tokio::task::spawn_blocking`.

### 3.5 Note Writer

```rust
pub struct NoteWriter {
    vault_root: PathBuf,
}

impl NoteWriter {
    /// Create a new note. Returns the path written.
    /// Fails if the note already exists (use update for that).
    pub fn create(
        &self,
        path: &Path,           // relative to vault root
        body: &str,
        frontmatter: Frontmatter,
    ) -> Result<PathBuf>;

    /// Update an existing note's body, preserving/merging frontmatter.
    pub fn update(
        &self,
        path: &Path,
        body: &str,
        frontmatter_updates: FrontmatterPatch,
    ) -> Result<()>;

    /// Atomic write: write to .tmp, fsync, rename.
    /// This prevents partial writes on crash.
    fn atomic_write(&self, path: &Path, content: &str) -> Result<()>;
}
```

**Critical: Atomic writes.** We write to `<path>.tmp`, call `fsync`, then `rename`. This ensures that a crash mid-write never leaves a corrupted note. The rename is atomic on all modern filesystems.

**Frontmatter round-tripping:** When updating a note, we parse the existing frontmatter, apply patches, and re-serialize. The `#[serde(flatten)] extra` field ensures we don't lose user-defined fields. We use `serde_yaml` for serialization, preserving key order where possible.

---

## 4. MCP Server — Detailed Tool Definitions

### 4.1 Transport

Support two transports, selected at startup:

- **stdio** (default): For Claude Desktop. Read JSON-RPC from stdin, write to stdout. Simple, reliable.
- **SSE**: For Claude web / remote access. HTTP server (via `axum`) with Server-Sent Events. Needed for the "turn this answer into notes" use case from claude.ai.

### 4.2 MCP Tools

```json
[
  {
    "name": "vault_search",
    "description": "Search the Obsidian vault using full-text and/or semantic search. Returns matching notes with snippets. Use this to find existing knowledge before creating new notes.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Search query (natural language or keywords)"
        },
        "mode": {
          "type": "string",
          "enum": ["auto", "fts", "vector", "hybrid"],
          "default": "auto"
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "maximum": 50
        },
        "tags": {
          "type": "array",
          "items": { "type": "string" },
          "description": "Filter to notes with ALL of these tags"
        },
        "path_prefix": {
          "type": "string",
          "description": "Filter to notes under this folder (e.g., 'research/')"
        }
      },
      "required": ["query"]
    }
  },
  {
    "name": "vault_read",
    "description": "Read the full content of a note, including frontmatter. Use this to get complete context from a search result.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Note path relative to vault root (e.g., 'research/quantum.md')"
        }
      },
      "required": ["path"]
    }
  },
  {
    "name": "vault_create",
    "description": "Create a new note in the vault with proper frontmatter. AI metadata is automatically added. Use for creating research notes, summaries, and new content.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Path for the new note (e.g., 'research/ai/transformers.md'). Directories are created automatically."
        },
        "title": {
          "type": "string",
          "description": "Note title"
        },
        "body": {
          "type": "string",
          "description": "Markdown content of the note"
        },
        "tags": {
          "type": "array",
          "items": { "type": "string" },
          "description": "Tags to apply"
        },
        "sources": {
          "type": "array",
          "items": { "type": "string" },
          "description": "Source URLs or vault paths used to generate this note"
        }
      },
      "required": ["path", "title", "body"]
    }
  },
  {
    "name": "vault_update",
    "description": "Update an existing note. Can append content, replace body, or update frontmatter fields.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string"
        },
        "body": {
          "type": "string",
          "description": "New body content (replaces existing)"
        },
        "append": {
          "type": "string",
          "description": "Content to append to the existing body"
        },
        "add_tags": {
          "type": "array",
          "items": { "type": "string" }
        },
        "remove_tags": {
          "type": "array",
          "items": { "type": "string" }
        }
      },
      "required": ["path"]
    }
  },
  {
    "name": "vault_list",
    "description": "List notes in the vault, optionally filtered by path or tags. Returns paths and titles.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "path_prefix": {
          "type": "string",
          "description": "List notes under this folder"
        },
        "tags": {
          "type": "array",
          "items": { "type": "string" }
        },
        "ai_generated": {
          "type": "boolean",
          "description": "Filter to AI-generated (true) or human-authored (false) notes"
        },
        "limit": {
          "type": "integer",
          "default": 50
        }
      }
    }
  },
  {
    "name": "vault_stats",
    "description": "Get vault statistics: total notes, tags, AI-generated counts, index health.",
    "inputSchema": {
      "type": "object",
      "properties": {}
    }
  }
]
```

### 4.3 MCP Resources

Expose notes as MCP resources so Claude can reference them directly:

```
note://vault/research/quantum.md     → Note content
index://vault/stats                   → Vault statistics
search://vault?q=transformers         → Search results
```

---

## 5. Agent Layer — "Note Librarian"

### 5.1 Agent Loop (Core)

```rust
/// The fundamental agent loop. All tasks (research, summarize, organize)
/// use this same loop with different system prompts and tool sets.
pub async fn agent_loop(
    llm: &dyn LlmBackend,
    system_prompt: &str,
    initial_message: &str,
    tools: &[ToolDef],
    tool_executor: &dyn ToolExecutor,
    config: &AgentConfig,
) -> Result<AgentResult> {
    let mut messages: Vec<Message> = vec![
        Message::user(initial_message),
    ];

    let mut iterations = 0;
    let max_iterations = config.max_iterations; // default: 20

    loop {
        iterations += 1;
        if iterations > max_iterations {
            tracing::warn!("Agent hit max iterations ({max_iterations}), stopping");
            break;
        }

        // SPAN: "agent_iteration" with iteration number
        let _span = tracing::info_span!("agent_iteration", iter = iterations).entered();

        // Call LLM
        let response = llm.chat(system_prompt, &messages, tools).await?;

        // Check stop reason
        match response.stop_reason {
            StopReason::EndTurn => {
                // Agent is done
                messages.push(Message::assistant(response.content.clone()));
                break;
            }
            StopReason::ToolUse => {
                // Execute each tool call
                messages.push(Message::assistant(response.content.clone()));

                let mut tool_results = vec![];
                for tool_call in response.tool_calls() {
                    let _tool_span = tracing::info_span!(
                        "tool_execution",
                        tool = tool_call.name,
                    ).entered();

                    let result = tool_executor.execute(
                        &tool_call.name,
                        &tool_call.input,
                    ).await?;

                    tool_results.push(ToolResult {
                        tool_use_id: tool_call.id.clone(),
                        content: result,
                    });
                }

                messages.push(Message::tool_results(tool_results));
            }
            other => {
                tracing::warn!("Unexpected stop reason: {:?}", other);
                break;
            }
        }
    }

    Ok(AgentResult {
        messages,
        iterations,
        // notes_created, notes_modified tracked by tool_executor
    })
}
```

### 5.2 Research Task

The primary use case. User says: `vault-mcp research "quantum error correction"`

**System prompt (research task):**

```
You are a research librarian managing an Obsidian knowledge vault. Your job
is to research the given topic and create well-organized notes.

## Process
1. ALWAYS search the vault first to understand existing knowledge.
2. Identify gaps — what's missing or outdated?
3. Create new notes to fill those gaps. Each note should be:
   - Focused on a single concept or sub-topic
   - Written in clear, concise markdown
   - Linked to related existing notes using [[wikilinks]]
   - Tagged appropriately
4. If existing notes need updates, update them.
5. Create an index/MOC (Map of Content) note linking everything together.

## Rules
- Prefer depth over breadth. 3 excellent notes > 10 shallow ones.
- Use proper heading hierarchy (# > ## > ###).
- Include specific examples, not just abstract descriptions.
- Cite sources in the note body when drawing from web search.
- Every note you create gets automatic AI metadata in frontmatter.
- Stop when you've created a useful, navigable knowledge cluster.

## Available Tools
- vault_search: Find existing notes
- vault_read: Read a specific note
- vault_create: Create a new note
- vault_update: Update an existing note
- vault_list: Browse the vault
```

### 5.3 LLM Backend Trait

```rust
#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn chat(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<LlmResponse>;

    fn model_name(&self) -> &str;
    fn provider_name(&self) -> &str;
}

/// Config-driven backend selection
pub fn create_backend(config: &LlmConfig) -> Result<Box<dyn LlmBackend>> {
    match config.provider.as_str() {
        "anthropic" => Ok(Box::new(AnthropicBackend::new(
            &config.api_key,
            &config.model,  // default: "claude-sonnet-4-5-20250929"
        )?)),
        "openai" => Ok(Box::new(OpenAiBackend::new(
            &config.api_key,
            &config.model,  // default: "gpt-4o"
        )?)),
        "ollama" => Ok(Box::new(OllamaBackend::new(
            &config.endpoint, // default: "http://localhost:11434"
            &config.model,    // default: "llama3.1"
        )?)),
        other => Err(anyhow!("Unknown LLM provider: {other}")),
    }
}
```

---

## 6. Observability — Profiling from Day One

### 6.1 Tracing Strategy

Use `tracing` crate throughout. Every significant operation gets a span.

```rust
// In search.rs
#[tracing::instrument(skip(self), fields(results = tracing::field::Empty))]
pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
    let _fts_span = tracing::info_span!("fts5_query").entered();
    let fts_results = self.fts_search(&query.text, query.limit * 2)?;
    drop(_fts_span);

    let _vec_span = tracing::info_span!("vector_query").entered();
    let vec_results = self.vector_search(&query.text, query.limit * 2)?;
    drop(_vec_span);

    let _rrf_span = tracing::info_span!("rrf_merge").entered();
    let merged = reciprocal_rank_fusion(&fts_results, &vec_results, query.limit);

    tracing::Span::current().record("results", merged.len());
    Ok(merged)
}
```

### 6.2 Profiling Subscribers

```rust
// In main.rs / lib.rs setup
pub fn init_tracing(config: &TracingConfig) {
    let subscriber = tracing_subscriber::registry();

    // Always: structured logs
    let subscriber = subscriber.with(
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_level(true)
    );

    // Optional: JSON log output for analysis
    if config.json_logs {
        // ... add JSON layer
    }

    // Optional: Tracy integration for flamegraphs
    #[cfg(feature = "profiling")]
    let subscriber = subscriber.with(tracing_tracy::TracyLayer::default());

    // Optional: Chrome tracing format (open in chrome://tracing)
    #[cfg(feature = "profiling")]
    let subscriber = subscriber.with(
        tracing_chrome::ChromeLayerBuilder::new()
            .file("trace.json")
            .build()
    );

    subscriber.init();
}
```

### 6.3 Built-in Benchmark Command

```
$ vault-mcp profile --vault ~/my-vault

╭─────────────────────────────────────────────────╮
│            vault-mcp profiling report            │
├─────────────────────────────────────────────────┤
│ Vault: ~/my-vault (3,847 notes)                 │
│                                                  │
│ Indexing                                         │
│   Full index:         1,243ms (3,094 notes/sec)  │
│   Incremental (1):       18ms                    │
│   Incremental (10):      47ms                    │
│                                                  │
│ Search (100 random queries)                      │
│   FTS5      p50:  2.1ms  p95:  4.8ms  p99:  8ms │
│   Vector    p50:  5.3ms  p95: 12.1ms  p99: 18ms │
│   Hybrid    p50:  6.8ms  p95: 14.2ms  p99: 22ms │
│                                                  │
│ Write                                            │
│   Create note:        3.2ms                      │
│   Update note:        4.1ms                      │
│   Atomic write:       1.8ms (fsync'd)            │
│                                                  │
│ Memory                                           │
│   Index RSS:         48 MB                       │
│   Per-note overhead:  ~12 KB                     │
╰─────────────────────────────────────────────────╯
```

This is a real CLI command, not just a dev tool. It runs `criterion`-style benchmarks against the actual vault and prints results. Ship it.

### 6.4 Cargo Feature Flags for Profiling

```toml
[features]
default = []
profiling = ["tracing-tracy", "tracing-chrome"]
```

When building for development: `cargo build --features profiling`
Release builds omit profiling overhead by default.

---

## 7. CLI Design

```
vault-mcp — Fast Obsidian MCP server with AI research agent

USAGE:
    vault-mcp <COMMAND>

COMMANDS:
    serve       Start the MCP server (stdio or SSE)
    search      Search the vault (full-text, vector, or hybrid)
    read        Read a note's content
    create      Create a new note
    index       Rebuild or update the vault index
    research    Run the AI research agent on a topic
    summarize   Summarize notes matching a query
    stats       Show vault statistics
    profile     Run performance benchmarks
    config      Show or edit configuration
    help        Print help

GLOBAL OPTIONS:
    --vault <PATH>        Vault root directory [env: VAULT_MCP_VAULT]
    --config <PATH>       Config file path [default: ~/.config/vault-mcp/config.toml]
    --log-level <LEVEL>   Log level [default: info]
    --json                Output as JSON (for scripting)
    -v, --verbose         Increase log verbosity
```

**CLI output:** Use `colored` or `owo-colors` for terminal output. Default to human-readable pretty output; `--json` for machine-readable. This makes the CLI scriptable while keeping it pleasant for interactive use.

**Example interactions:**

```bash
# Fast search
$ vault-mcp search "transformer attention mechanism"
  📄 research/ai/attention-is-all-you-need.md (score: 0.94)
     ...multi-head attention allows the model to jointly attend...
  📄 research/ai/vision-transformers.md (score: 0.78)
     ...adapted the transformer architecture for image patches...

# Research agent
$ vault-mcp research "Rust async runtime internals" --provider anthropic
  🔍 Searching vault for existing notes...
  📖 Found 3 related notes
  🤖 Researching with claude-sonnet-4-5-20250929...
  ✏️  Created: research/rust/async-runtime-overview.md
  ✏️  Created: research/rust/tokio-internals.md
  ✏️  Created: research/rust/waker-mechanism.md
  ✏️  Updated: research/rust/index.md (added links)
  ✅ Done (4 iterations, 3 notes created, 1 updated)

# Start MCP server for Claude Desktop
$ vault-mcp serve --transport stdio

# Start MCP server for remote access (Claude web)
$ vault-mcp serve --transport sse --port 8080
```

---

## 8. Configuration

```toml
# ~/.config/vault-mcp/config.toml

[vault]
path = "~/obsidian-vault"
# Folders to exclude from indexing
exclude = [".obsidian", ".trash", "templates"]

[index]
# Where to store the SQLite database
db_path = "~/.local/share/vault-mcp/index.db"
# Debounce window for file watcher (ms)
watch_debounce_ms = 500

[search]
# Default search mode
default_mode = "auto" # auto | fts | vector | hybrid
# Number of results
default_limit = 20

[embeddings]
# Provider for generating embeddings
provider = "openai" # openai | ollama | voyage | none
model = "text-embedding-3-small"
# For ollama:
# provider = "ollama"
# model = "nomic-embed-text"
# endpoint = "http://localhost:11434"
dimension = 1536
# Batch size for embedding generation
batch_size = 100

[llm]
# Provider for the research agent
provider = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_env = "ANTHROPIC_API_KEY" # Read from this env var
# For ollama:
# provider = "ollama"
# model = "llama3.1:70b"
# endpoint = "http://localhost:11434"
max_agent_iterations = 20

[mcp]
# Server transport
transport = "stdio" # stdio | sse
# SSE-specific
sse_port = 8080
sse_host = "127.0.0.1"

[profiling]
enabled = false
output = "trace.json" # Chrome tracing format
tracy = false # Tracy profiler integration

[notes]
# Template for AI-generated note frontmatter
# Notes always get ai metadata; this controls additional defaults
default_tags = ["ai-generated", "needs-review"]
# Folder for research outputs (can be overridden per-command)
research_folder = "research"
```

---

## 9. TUI Design (Phase 2)

Use `ratatui` with `crossterm` backend. Three main views:

### 9.1 Search View (Default)

```
╭─ vault-mcp ──────────────────────── 3,847 notes ─╮
│ 🔍 Search: transformer attention█                  │
│ Mode: [hybrid]  Tags: [none]                        │
├─────────────────────────────────────────────────────┤
│                                                     │
│  ▸ research/ai/attention-is-all-you-need.md  0.94   │
│    ...multi-head attention allows the model to...   │
│                                                     │
│    research/ai/vision-transformers.md         0.78   │
│    ...adapted the transformer architecture...       │
│                                                     │
│    research/ai/bert-architecture.md           0.71   │
│    ...self-attention mechanism in BERT...           │
│                                                     │
│    research/ai/flash-attention.md             0.65   │
│    ...IO-aware exact attention algorithm...         │
│                                                     │
├─────────────────────────────────────────────────────┤
│ ↑↓ navigate  Enter: read  Tab: mode  /: search     │
│ r: research  i: re-index  p: profile  q: quit      │
╰─────────────────────────────────────────────────────╯
```

### 9.2 Agent View (Live Progress)

```
╭─ vault-mcp agent ──────── research: "quantum error correction" ─╮
│                                                                   │
│  Iteration 3/20                                                   │
│                                                                   │
│  ┃ 🔍 vault_search("quantum error correction")                   │
│  ┃    → 2 results found                                          │
│  ┃ 📖 vault_read("research/quantum/basics.md")                   │
│  ┃    → 1,243 tokens                                             │
│  ┃ ✏️  vault_create("research/quantum/error-correction.md")       │
│  ┃    → Created (2,891 tokens)                                   │
│  ┃ 🤖 Thinking...                                                │
│  ┃    "Now I'll create a note on surface codes..."               │
│  ▸ ✏️  vault_create("research/quantum/surface-codes.md")          │
│       → Writing...                                                │
│                                                                   │
│  Created: 2 notes    Updated: 0    Tokens used: ~8,400           │
│                                                                   │
├───────────────────────────────────────────────────────────────────┤
│ Ctrl+C: stop agent    s: skip iteration    q: quit               │
╰───────────────────────────────────────────────────────────────────╯
```

### 9.3 Profile View

```
╭─ vault-mcp profile ──────────────────────────────────────────╮
│                                                               │
│  Search Latency (last 100 queries)                           │
│                                                               │
│  FTS5   ▏██████████████░░░░░░░░░░░░░░░░  p50: 2.1ms         │
│  Vector ▏████████████████████████░░░░░░░  p50: 5.3ms         │
│  Hybrid ▏██████████████████████████░░░░░  p50: 6.8ms         │
│                                                               │
│  Indexing                                                     │
│  Full   ▏████████████████████████████████ 1,243ms             │
│  Incr.  ▏██░░░░░░░░░░░░░░░░░░░░░░░░░░░░    18ms             │
│                                                               │
│  Memory: 48 MB RSS    DB size: 12 MB                         │
│  Notes: 3,847    Embeddings: 3,612 (94%)                     │
│                                                               │
├───────────────────────────────────────────────────────────────┤
│ r: re-run    e: export JSON    q: quit                       │
╰───────────────────────────────────────────────────────────────╯
```

---

## 10. Build Phases & Milestones

### Phase 1: Foundation (Week 1-2)

**Goal:** Working vault index + FTS5 search + CLI

- [ ] Cargo workspace setup with all crate stubs
- [ ] `vault-core`: Note struct, frontmatter parsing, round-trip serialization
- [ ] `vault-core`: SQLite schema, migrations, WAL mode
- [ ] `vault-core`: Full vault scan, content hashing, FTS5 indexing
- [ ] `vault-core`: File watcher with debounce
- [ ] `vault-core`: FTS5 search with snippet highlighting
- [ ] `vault-core`: Note writer with atomic writes
- [ ] `vault-cli`: `search`, `read`, `create`, `index`, `stats` commands
- [ ] `tracing` instrumentation on all core operations
- [ ] Integration tests with fixture vaults (10, 500 notes)
- [ ] **Milestone:** `vault-mcp search "query"` returns results in < 5ms on 500-note vault

### Phase 2: MCP Server (Week 3)

**Goal:** Working MCP server, usable from Claude Desktop

- [ ] `vault-mcp-server`: JSON-RPC over stdio transport
- [ ] `vault-mcp-server`: All 6 tool definitions (search, read, create, update, list, stats)
- [ ] `vault-mcp-server`: Tool execution → vault-core delegation
- [ ] `vault-mcp-server`: MCP resources (note:// URIs)
- [ ] Test with Claude Desktop
- [ ] `vault-cli`: `serve` command
- [ ] **Milestone:** Say "search my vault for X" in Claude Desktop and get results

### Phase 3: Agent (Week 4)

**Goal:** Research agent that creates multi-note knowledge clusters

- [ ] `vault-agent`: LlmBackend trait + Anthropic implementation
- [ ] `vault-agent`: Core agent loop (tool-use cycle)
- [ ] `vault-agent`: Research task with system prompt
- [ ] `vault-agent`: AI metadata injection in all created notes
- [ ] `vault-agent`: Session tracking (link related notes)
- [ ] `vault-cli`: `research` command with live progress output
- [ ] OpenAI backend
- [ ] Ollama backend
- [ ] **Milestone:** `vault-mcp research "topic"` creates 3+ linked notes with proper metadata

### Phase 4: Vector Search (Week 5)

**Goal:** Semantic search, hybrid search, embeddings pipeline

- [ ] `vault-core`: sqlite-vec integration
- [ ] `vault-core`: EmbeddingBackend trait + OpenAI/Ollama implementations
- [ ] `vault-core`: Async embedding pipeline (background, batched)
- [ ] `vault-core`: Hybrid search with RRF
- [ ] `vault-core`: Embedding dimension configuration
- [ ] Update all search callers to use hybrid mode
- [ ] **Milestone:** "find notes about machine learning" matches "deep learning architectures" note

### Phase 5: Profiling & Performance (Week 6)

**Goal:** Comprehensive profiling, benchmark suite, optimization

- [ ] `vault-cli`: `profile` command with formatted output
- [ ] Criterion benchmarks for indexing, FTS, vector, hybrid search
- [ ] Test vault generator script (1k, 5k, 10k notes)
- [ ] Chrome tracing export
- [ ] Tracy integration (behind feature flag)
- [ ] Identify and fix any operations > target latency
- [ ] Connection pooling if needed (r2d2 or deadpool)
- [ ] **Milestone:** All perf targets met (see §3.3 table)

### Phase 6: TUI (Week 7-8)

**Goal:** Beautiful terminal UI for interactive use

- [ ] `vault-tui`: ratatui app scaffold
- [ ] Search view with live query
- [ ] Note preview/read view
- [ ] Agent view with streaming progress
- [ ] Profile dashboard view
- [ ] Keyboard navigation
- [ ] **Milestone:** Full interactive TUI session: search → read → research → view results

### Phase 7: SSE + Polish (Week 8+)

**Goal:** Remote MCP, documentation, release

- [ ] SSE transport (axum-based)
- [ ] "Save to vault" from Claude web
- [ ] Summarize task for agent
- [ ] Organize task for agent (re-tag, re-link)
- [ ] README with demo GIFs
- [ ] `cargo install` support
- [ ] CI/CD (GitHub Actions: test, lint, build, release binaries)

---

## 11. Testing Strategy

### Unit Tests

Every module has unit tests. Key areas:

- Frontmatter parsing: round-trip fidelity, malformed YAML recovery, `extra` field preservation
- FTS5 query building: tokenization, escaping special characters
- RRF: correctness with edge cases (empty result sets, single-source results)
- Atomic write: simulated crash during write doesn't corrupt

### Integration Tests

Test against real SQLite databases with fixture vaults:

- Full index → search → verify results
- Create note → search → find it
- File watcher: modify file on disk → verify index updates
- MCP server: send JSON-RPC requests → verify tool responses

### Performance Tests (Criterion)

Run on every PR. Alert if any benchmark regresses > 10%:

- `bench_index_full_500` — Full index of 500-note vault
- `bench_index_full_5000` — Full index of 5,000-note vault
- `bench_index_incremental` — Single file change re-index
- `bench_search_fts_simple` — Simple keyword search
- `bench_search_fts_complex` — Multi-term with filters
- `bench_search_vector` — Vector similarity search
- `bench_search_hybrid` — Full hybrid search with RRF
- `bench_note_create` — Create note with frontmatter

### Fixture Vault Generator

```python
# scripts/generate_test_vault.py
# Generates synthetic vaults with realistic structure:
# - Nested folders (research/, daily/, projects/)
# - Wikilinks between notes (random graph, ~3 links per note)
# - Realistic frontmatter (tags, dates, some with AI metadata)
# - Varying note sizes (100 words to 5,000 words)
# - Some notes with code blocks, tables, images
# Sizes: 10, 100, 500, 1000, 5000, 10000
```

---

## 12. Key Dependencies (Cargo.toml)

```toml
[workspace]
members = [
  "crates/vault-core",
  "crates/vault-mcp-server",
  "crates/vault-agent",
  "crates/vault-cli",
  "crates/vault-tui",
]

# Shared dependencies (workspace-level)
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }

# vault-core specific
rusqlite = { version = "0.32", features = ["bundled", "vtab"] }
walkdir = "2"
notify = "7"
notify-debouncer-mini = "0.5"
pulldown-cmark = "0.12"
xxhash-rust = { version = "0.8", features = ["xxh3"] }
rayon = "1"
reqwest = { version = "0.12", features = ["json"] }
async-trait = "0.1"

# vault-cli specific
clap = { version = "4", features = ["derive"] }
colored = "2"
indicatif = "0.17" # Progress bars

# vault-tui specific
ratatui = "0.29"
crossterm = "0.28"

# vault-mcp-server specific
axum = "0.8" # For SSE transport
tokio-stream = "0.1"

# Profiling (optional)
tracing-chrome = { version = "0.7", optional = true }
# tracing-tracy = { version = "0.11", optional = true }

# Testing
[workspace.dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
tempfile = "3"
```

---

## 13. Error Handling Philosophy

Use `thiserror` for library errors (structured, matchable). Use `anyhow` in binaries (CLI, MCP server) for ergonomic error chains.

```rust
// In vault-core/src/errors.rs
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Note not found: {path}")]
    NoteNotFound { path: PathBuf },

    #[error("Note already exists: {path}")]
    NoteAlreadyExists { path: PathBuf },

    #[error("Invalid frontmatter in {path}: {reason}")]
    InvalidFrontmatter { path: PathBuf, reason: String },

    #[error("Index error: {0}")]
    Index(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Embedding error: {reason}")]
    Embedding { reason: String },

    #[error("Search error: {reason}")]
    Search { reason: String },
}
```

**Error reporting in MCP:** Map `VaultError` variants to appropriate MCP error codes. Never expose internal details (file paths, SQL errors) to the LLM. Return human-readable messages.

---

## 14. Security Considerations

1. **API keys** are read from environment variables, never stored in config files. The config file stores the env var _name_, not the value.
2. **Vault path sandboxing:** The note writer refuses to write outside the vault root. All paths are canonicalized and checked.
3. **No network access from vault-core.** The core library is pure filesystem + SQLite. Network access (LLM APIs, embedding APIs) lives exclusively in `vault-agent` and `vault-mcp-server`.
4. **MCP SSE transport** binds to `127.0.0.1` by default. Remote access requires explicit `--host 0.0.0.0` flag with a warning.
5. **AI metadata is tamper-evident.** Notes include a content hash in frontmatter so you can detect if the body was modified after generation (useful for the CLASP trust model).

---

## 15. Future Extensions (Not in Scope Now, But Architected For)

- **Graph search:** Traverse wikilinks to find related notes N hops away. The `links` table supports this.
- **Obsidian plugin:** A companion Obsidian plugin that communicates with the MCP server for in-editor AI features.
- **Multi-vault support:** Run one server, index multiple vaults. The schema supports this with a `vault_id` column (add when needed).
- **Collaborative vaults:** Shared SQLite over LiteFS or similar. The WAL + atomic write architecture supports this.
- **Web search integration in agent:** Let the research agent search the web (via Tavily, Brave Search, or SearXNG) during research tasks.
- **PDF/image indexing:** Extract text from PDFs and OCR images in the vault. Index alongside markdown.
