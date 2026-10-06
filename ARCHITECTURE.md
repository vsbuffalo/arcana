# Architecture

## Crate structure

```
arcana/
├── arcana-core        library, zero UI deps
│   ├── config         global + vault-local TOML merge
│   ├── vault          Vault struct, owns SQLite connection
│   ├── index          FTS5 full-text search, incremental indexing
│   ├── note           markdown parsing, frontmatter, wikilinks
│   ├── profile        brain profile (taxonomy.md + style.md)
│   ├── drafts         draft sessions, approve/reject workflow
│   ├── writer         note creation with zone enforcement
│   └── git            provenance, dual-author commits
│
├── arcana-agent       LLM integration, zero UI deps
│   ├── backend/       LlmBackend trait + anthropic, openai, ollama
│   ├── chat           multi-turn chat session with tool use
│   ├── ingest         explore → plan → generate pipeline
│   ├── pricing        cost estimation per model
│   ├── tools          vault tool executor (search, read, draft)
│   ├── project_tools  external project tools (tree, read, search)
│   ├── prompt         composable system prompt assembly
│   └── context        vault context generation for cross-linking
│
├── arcana-server      MCP server (stdio, streamable HTTP, legacy SSE)
├── arcana-cli         thin clap CLI, owns all UX
└── arcana-tui         (placeholder)
```

## Data flow

```
            ┌──────────────┐
            │   Obsidian   │  vault/*.md on disk
            │    vault     │
            └──────┬───────┘
                   │ index (rayon parallel, xxhash change detection)
            ┌──────▼───────┐
            │    SQLite    │  FTS5 + metadata + tags + wikilinks
            │   index.db   │  WAL mode, single connection
            └──────┬───────┘
                   │
      ┌────────────┼────────────┐
      │            │            │
┌─────▼─────┐ ┌───▼───┐ ┌─────▼──────┐
│  CLI/MCP   │ │ Chat  │ │   Ingest   │
│  search    │ │ agent │ │ pipelines  │
│  read/list │ │       │ │            │
└────────────┘ └───┬───┘ └─────┬──────┘
                   │           │
            ┌──────▼───────┐   │
            │ LLM Backend  │◄──┘
            │  anthropic   │
            │  openai      │
            │  ollama      │
            └──────┬───────┘
                   │
            ┌──────▼───────┐
            │    Drafts    │  .arcana/drafts/{session}/
            │   staging    │  approve/reject via `arcana review`
            └──────────────┘
```

## Config hierarchy

```
compiled defaults (config.rs)
    └─▸ ~/.config/arcana/config.toml     global
         └─▸ <vault>/.arcana/config.toml vault-local
              └─▸ CLI flags              highest priority
```

Deep TOML table merge: a vault config with just `[agent.ingest]` won't
clobber global `[llm]` settings.

## Pipeline architecture

### Ingest: explore → plan → generate

```
┌───────────┐     ┌───────────┐     ┌───────────┐
│  Explore  │────▸│   Plan    │────▸│ Generate  │
│  (agent)  │     │ (single)  │     │  (batch)  │
└───────────┘     └───────────┘     └───────────┘
 tool-use loop     JSON output       1 call/note
 project_tree      note list         write markdown
 project_read      source mapping    inject AI meta
 vault_search                        create drafts
```

**Explore** is an agentic multi-turn loop: the LLM reads project files via
tools, checks the vault for duplicates, and produces a `<summary>`.

**Plan** takes the summary + vault context and outputs a structured JSON
plan listing notes to create, their paths, titles, and source files.

**Generate** iterates over the plan. For each note, re-reads source files
and generates formatted markdown with frontmatter, wikilinks, and AI
provenance metadata.

### Engine API

The pipeline exposes a phased engine struct (`IngestEngine`)
where each phase is a separate method returning a typed result. The CLI
creates the engine, calls phases sequentially, and owns the interactive
prompt between plan and generate. `run_ingest()` is a thin
convenience wrappers that call all phases — used by tests and `--auto`.

### Phase control

Planning is the default stop point. Generation is opt-in via interactive
prompt or `--auto` for unattended mode:

```
explore ──▸ plan ──▸ [g]enerate / [e]dit / [q]uit
                         │
                         ▼
                    generate ──▸ drafts ──▸ arcana review
```

## Draft system

AI output never goes directly into the vault. Everything flows through a
staging area in `.arcana/drafts/`, organized by session.

### Storage layout

```
vault_root/
└── .arcana/
    └── drafts/
        └── a1b2c3d4/                    # session (8-char UUID)
            ├── _manifest.toml           # session metadata + per-draft status
            ├── concepts/mcmc.md         # draft mirrors its final vault path
            └── projects/starsim/cal.md
```

Draft files are stored at their final relative path within the session
directory — the directory structure mirrors the vault structure exactly.

### Session manifest (`_manifest.toml`)

Each session tracks metadata and per-draft status:

```toml
id = "a1b2c3d4"
created_at = "2025-03-04T15:30:00Z"
source = "ingest" # ingest | chat | mcp
provider = "anthropic"
model = "claude-sonnet-4-5-20250929"
task = "ingest myproject"
input_hash = "abc123..." # for dedup across runs

[[drafts]]
path = "concepts/mcmc.md"
status = "pending" # pending | approved | rejected | edited
kind = "new_note" # new_note | suggest_edit
```

### Lifecycle

```
create_session()          session dir + _manifest.toml
    │
create_draft()            write file, zone-validate, update manifest
    │                     (status: pending)
    │
arcana review             show diff + preview for each pending draft
    ├── [a]pprove         move to vault, reindex, git commit (AI author)
    ├── [d]elete          reject, remove draft file
    ├── [s]kip            leave pending
    └── [A]pprove all     approve remaining
    │
prune                     delete resolved sessions > retention_days
```

### Zone validation

`create_draft()` validates the target path against taxonomy zones.
`suggest_edit()` (used by chat) skips zone validation — it targets
existing vault notes already in valid locations.

The CLI `edit_plan()` also validates zones when the user edits a plan
in `$EDITOR`, with helpful error messages and automatic editor reopen.

### Conflict detection

Before generating drafts, ingest calls `find_conflicts()`
to check for pending sessions targeting the same output paths. Duplicate
runs are detected via `input_hash`.

### On approval

1. Draft content written to `vault_root/<path>`
2. Draft file deleted from session directory
3. Manifest status updated to `approved`
4. Vault reindexes affected paths (FTS5 + metadata)
5. Git commits with AI author identity (`arcana-ai <ai@arcana.local>`)

## LLM backend

```rust
#[async_trait]
trait LlmBackend: Send + Sync {
    async fn chat(&self, system: &str, messages: &[Message],
                  tools: &[ToolDef]) -> Result<LlmResponse>;
    fn model_name(&self) -> &str;
    fn provider_name(&self) -> &str;
}
```

Three implementations:

- **Anthropic**: Messages API, native tool use
- **OpenAI**: Chat Completions API (also used for compatible providers)
- **Ollama**: OpenAI-compatible endpoint on localhost

All backends support configurable `max_output_tokens` and exponential
backoff retry.

## Prompt assembly

System prompts are composed from five optional sections, in order:

1. **Taxonomy** — vault zone definitions, routing rules
2. **Style guide** — formatting, voice, templates
3. **Domain skill** — task-specific extraction methodology
4. **Task** — what to do now (explore, plan, generate, chat)
5. **Vault context** — existing notes for cross-link awareness

Order matters: models process tokens sequentially, so structural context
(how the vault is organized) comes before behavioral instructions.

## Git provenance

Uses git2 (libgit2 bindings) for all git operations. Two identities:

```
author    = user (from git config or [git] settings)
committer = arcana-ai <ai@arcana.local>
```

AI-authored commits use the AI identity for both author and committer.
Human edits use the human identity. This makes `git blame` and `git log
--author` useful for distinguishing who wrote what.

```
┌─────────────────┐     ┌──────────────────┐
│  arcana review   │────▸│ commit_ai_write  │  AI as author
│  (approve draft) │     │ ai@arcana.local  │
└─────────────────┘     └──────────────────┘

┌─────────────────┐     ┌──────────────────┐
│  user edits .md  │────▸│commit_human_change│  human as author
│  in Obsidian     │     │ user@email.com   │
└─────────────────┘     └──────────────────┘
```

### Per-line provenance

`blame()` walks git blame output and classifies each line by author email:

- `@arcana.local` → `ProvenanceAuthor::Ai`
- anything else → `ProvenanceAuthor::Human`

Produces `NoteProvenance` with `human_lines`, `ai_lines`, `human_pct`,
`ai_pct` — lets the UI show what percentage of a note is AI-generated.

### Dedup guard

AI writes are tracked in an in-memory `pending_ai_writes` Mutex.
`commit_human_change()` filters these out so a file the AI just wrote
isn't immediately re-attributed as a human edit when the index re-runs.

### Config

```toml
[git]
enabled = true
auto_commit = true
ai_name = "arcana-ai"
ai_email = "ai@arcana.local"
# user_name / user_email fall back to git config
```

## Key design decisions

- **Single SQLite connection** with `query_only` pragma toggling
- **FTS5** for full-text search, not an external search engine
- **xxhash** for incremental change detection (skip unchanged files)
- **rayon** for parallel markdown parsing during indexing
- **Drafts as staging** — AI never writes directly to vault files
- **AI provenance** — every AI-generated note carries `ai:` frontmatter
- **Brain profile** — user taxonomy + style guide shapes all LLM output
- **Zone enforcement** — paths validated against taxonomy before draft creation
