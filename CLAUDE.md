# Arcana

Obsidian vault indexer and search CLI, built in Rust.

## Design Goals

1. **Speed** — sub-10ms search on 10k+ note vaults
2. **Pipe-friendly CLI** — when stdout is not a TTY, output plain paths (one per line) so results compose with `grep`, `xargs`, `fzf`, `head`, etc. Pretty output is for humans in terminals only. `--json` and `--paths` flags force machine-readable output regardless of TTY.
3. **Correctness** — no silent data loss, crash-safe atomic writes, ACID via SQLite WAL
4. **Local-first** — everything works offline, vault never leaves the machine

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
