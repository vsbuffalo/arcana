# Pre-open-source security & engineering review

Date: 2026-06-12
Scope: full workspace, ahead of making the repository public
Branch (batch 1 fixes): `security/path-traversal-and-bind`

## Context

Arcana is being prepared for public release. This review focused on (1) anything
that must not ship in a network-facing, AI-driven tool, and (2) AI-engineering
correctness and cost. The retrieval core (FTS5 with bound parameters, a separate
FTS5 query sanitizer, atomic same-directory writes) and the overall architecture
(typestate pipelines, trait-based backend seam, draft staging, provenance) are
sound; findings below are specific and, for batch 1, fixed.

## Batch 1 — path traversal & insecure-by-default binding (fixed here)

### Finding 1 (Critical): arbitrary file read / write / delete via unsanitized paths

`Vault::read_note`, `write_note_content`, and `delete_note` joined a
caller-supplied path onto the vault root with no validation
(`self.root.join(rel_path)`). `Path::join` discards the root for an absolute
argument (`root.join("/etc/passwd") == "/etc/passwd"`) and `..` escapes it. These
sinks are reachable from the MCP tools and the REST routes (`/api/notes/{*path}`
GET/PUT/DELETE, `/api/provenance/{*path}`), and reachable even over stdio via a
prompt-injected or confused model. The only existing guard, `NoteWriter::safe_path`,
used a substring `..` check (which both over- and under-matches) and was wired
into create/update only. `DraftManager::validate_path` and the git
`blame`/`restore` path arguments had the same class of gap.

**Fix — one validated chokepoint.** Added `arcana_core::vault_path::VaultPath`, a
newtype whose only constructor (`resolve(root, rel)`) rejects absolute paths and
`..`/root components lexically, then confirms containment by canonicalizing the
deepest existing ancestor (defeating symlink escapes for not-yet-existing
targets). Every filesystem sink now routes through it:

- `read_note` / `write_note_content` / `delete_note` resolve first.
- `NoteWriter::safe_path` returns a `VaultPath`; `atomic_write` now *takes* a
  `&VaultPath`, so a write cannot reach an unvalidated path — the illegal state
  is unrepresentable at the type level.
- `DraftManager::validate_path` uses `VaultPath::resolve`.
- `git::blame` / `git::restore` validate the path lexically before touching disk.

Tests cover normal paths, nested not-yet-existing paths, `..` traversal, absolute
paths, empty paths, and a Unix symlink-escape case.

### Finding 2 (High): insecure-by-default network binding

`serve_sse` always bound `0.0.0.0` and, with no auth flags, logged a warning and
served anyway — so the default `serve --transport sse` exposed the vault (and,
through Finding 1, the host filesystem) to the LAN unauthenticated.

**Fix.** Added a `--host` flag (env `ARCANA_HOST`) defaulting to `127.0.0.1`, and
made the server *fail closed*: it refuses to bind a non-loopback address unless a
bearer token or OAuth is configured. Loopback without auth is still permitted
(local use) but the address is no longer `0.0.0.0` by default.

### Finding 3 (housekeeping): missing LICENSE

The README declared MIT but no `LICENSE` file existed (so the repo was
effectively all-rights-reserved). Added the MIT license text.

## Verification

- `cargo build --workspace`: clean.
- `cargo test --workspace`: 215 pass. One failure,
  `git::tests::open_or_init_errors_without_identity`, is **pre-existing**
  uncommitted WIP — it fails identically on the pre-batch-1 tree and is unrelated
  to these changes.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.

## Not in batch 1

The secrets/PII scan was clean (no keys or PII in the working tree or full git
history; API keys are resolved by env-var name only and never logged). Remaining
AI-engineering, OAuth-hardening, DoS, and architecture findings are recorded in
`todo.md` at the repo root for follow-up.
