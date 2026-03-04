# Arcana

Obsidian vault indexer and search CLI, built in Rust.

## Design Goals

1. **Speed** — sub-10ms search on 10k+ note vaults
2. **Pipe-friendly CLI** — when stdout is not a TTY, output plain paths (one per line) so results compose with `grep`, `xargs`, `fzf`, `head`, etc. Pretty output is for humans in terminals only. `--json` and `--paths` flags force machine-readable output regardless of TTY.
3. **Correctness** — no silent data loss, crash-safe atomic writes, ACID via SQLite WAL
4. **Local-first** — everything works offline, vault never leaves the machine

## Workflows

Arcana's AI features follow a plan-first, human-in-the-loop workflow. Planning is cheap; generation is expensive. Every AI pipeline works the same way:

1. **Gather** — read inputs (project files or vault notes), build context
2. **Plan** — AI proposes what to create, show the plan and estimated cost
3. **Decide** — user reviews: generate, edit the plan in $EDITOR, or quit
4. **Generate** — AI writes drafts to `.arcana/drafts/<session>/`
5. **Review** — `arcana review` to approve/reject/edit each draft before it enters the vault

`--auto` skips the interactive prompt (step 3) for scripting/CI.

### Ingest (external project → vault notes)

Reads an external codebase and **authors new knowledge notes from scratch**. The AI explores the project, understands its architecture and concepts, then writes self-contained vault notes. Source material is code; output is explanatory notes.

### Tidy (inbox → structured notes)

Takes existing messy vault notes and **reorganizes them** — moves, splits, extracts concepts. The AI reads the source notes and rewrites them into the vault's taxonomy. Source material is vault notes; output is restructured vault notes.

### Skills

Skills are domain-specific instructions (markdown files in `.arcana/skills/`) injected into the AI system prompt. They teach the AI *how* to extract knowledge for a particular domain — e.g. a `model-extract` skill that knows how to identify model equations, parameters, and assumptions from scientific code.

Skills apply to `ingest` today (`--skill <name>`). The engine supports them for `tidy` too but the CLI doesn't expose it yet.

### Review

All AI output lands in drafts, never directly in the vault. `arcana review` shows diffs and lets you accept, reject, or edit before committing. Git tracks provenance (human vs AI authorship) per line.

## Architecture

- `arcana-core`: library crate, zero UI deps. `Vault` struct owns the DB and exposes all operations.
- `arcana-cli`: thin clap CLI over arcana-core. Detects TTY for output mode.
- Single SQLite connection with `query_only` pragma toggling for read/write boundaries.
- FTS5 for full-text search, xxhash for incremental change detection, rayon for parallel parsing.

## Conventions

- `cargo clippy -- -D warnings` must pass (CI enforces this)
- `cargo fmt --check` must pass
- MSRV: 1.80
- Edition: 2021
- Commit messages: conventional-ish, short lowercase subject with bullet-point body listing features and internals. See git log for examples.
- Git: always rebase when possible, prefer linear history over merge commits.
