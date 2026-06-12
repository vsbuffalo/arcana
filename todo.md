# Arcana — Batch 2 work list

Pre-open-source improvements. **Batch 1 (security blockers) is already done** on
branch `security/path-traversal-and-bind` — see
`docs/dev/reviews/2026-06-12-pre-oss-security-review.md`. This file is the
follow-up work: AI-engineering correctness/cost fixes, remaining security
hardening, and architecture cleanup.

**Definition of done for every task:** `cargo test --workspace` stays green
(except the *pre-existing* `git::tests::open_or_init_errors_without_identity`
failure, which is unrelated WIP — leave it or fix separately), and
`cargo clippy --workspace --all-targets -- -D warnings` is clean. Add a focused
test for each behavioral change.

---

## A. AI-engineering correctness & cost (do these first)

### A1. Prompt caching — biggest cost lever  ★high impact
The Anthropic backend never uses `cache_control`. The ingest/tidy *generate*
phase makes one API call per note sharing a large, byte-identical system prefix
(taxonomy + style + skill + tools), so that prefix is re-billed at full input
price on every call.
- `crates/arcana-agent/src/backend/anthropic.rs:38` — `system: &'a str` must
  become a structured content-block array so a `cache_control: {"type":
  "ephemeral"}` breakpoint can be placed on the last *static* block.
- Place the breakpoint after the static prefix (taxonomy/style/skill/task) and
  *before* the per-note `vault_context` (which is correctly appended last in
  `ingest.rs` / `tidy.rs` generate). Also mark the static **tool list** as
  cached (tools are invariant across a run).
- Parse and surface the cache token counts (see A4).
- **Acceptance:** a multi-note ingest reports cache *reads* on calls after the
  first; add a unit test asserting the request body carries a `cache_control`
  breakpoint on the static prefix.

### A2. Treat `StopReason::MaxTokens` as a first-class outcome
`agent.rs:128-143` and `chat.rs:114-119` fold `MaxTokens` into `EndTurn`. If the
cutoff lands mid-`tool_use`, the truncated tool call is appended to history with
no matching `tool_result`, so the *next* request 400s ("tool_use ids must have
corresponding tool_result").
- Detect `MaxTokens`; do **not** silently treat as done.
- If the truncated turn contains a `tool_use`, either drop it from history or
  append a synthetic error `tool_result`, and emit a distinct `Truncated`
  progress event so the caller knows output is incomplete.
- **Acceptance:** a mock backend returning `MaxTokens` mid-tool-call does not
  corrupt history; a test asserts no dangling `tool_use` remains.

### A3. Char-safe truncation (latent panic on your own output)
Byte-slicing `&s[..s.len().min(n)]` panics when byte `n` lands mid-UTF-8 char,
and the style guide tells the model to emit `—`/`→` (multi-byte).
- Replace at `ingest.rs:854`, `tidy.rs:854`, `tidy.rs:988`, `agent.rs:185`,
  `chat.rs:152` with a shared `fn truncate_chars(s: &str, n: usize) -> String {
  s.chars().take(n).collect() }` (you already use `chars().take` correctly in
  `agent.rs:89` / `tidy.rs:661`).
- **Acceptance:** a test truncating a string with em-dashes at a byte that
  splits a codepoint does not panic.

### A4. `Usage` must mirror the billing model; fix pricing
- `crates/arcana-agent/src/types.rs:85` — add `cache_creation_tokens` and
  `cache_read_tokens` to `Usage`.
- `backend/anthropic.rs:96` — parse `cache_creation_input_tokens` /
  `cache_read_input_tokens` (and the OpenAI equivalent if present).
- `pricing.rs` — price cache reads at input×0.1 and cache writes at input×1.25;
  key the price table on a `(family, generation)` enum instead of a bare
  `opus/sonnet/haiku` substring match (it silently goes stale across model
  generations today).
- **Acceptance:** cost estimate for a run with cache hits differs from the
  no-cache estimate; unit test on the new pricing math.

### A5. Harden structured-output (JSON) parsing
`util.rs:2-10` extracts JSON via first-`{`..last-`}`, which breaks on trailing
prose containing braces and can't tolerate code fences or trailing commas.
- Prefer depth-aware brace matching (ignore braces inside strings); strip
  ```json fences; on parse failure, reprompt **once** with the parse error fed
  back to the model.
- Even better where feasible: use the model's tool-calling / structured-output
  channel so the plan is a schema-validated object rather than parsed free text.
- **Acceptance:** tests for fenced JSON, JSON followed by a sentence with a `}`,
  and a trailing comma all parse (or trigger the single reprompt).

### A6. Separate spend budget from context-window budget
`chat.rs:63-205` grows `messages` unboundedly; the only guard is a cumulative
*spend* check (and it fires after results are appended). Long sessions grow the
resent context until the model's window rejects it.
- Add a context-window budget distinct from the spend cap; when estimated
  history tokens approach the window, compact (keep system prompt + recent turns
  + any open tool round, drop/summarize the middle).
- **Acceptance:** a session driven past the threshold compacts instead of
  growing without bound; test asserts history token estimate stays under a cap.

### A7. Retry/timeout robustness
`backend/mod.rs:24-47` correctly retries 429/5xx, but: it ignores `Retry-After`,
does **not** retry transport errors (`f().await?` propagates immediately — and
timeouts are the more common transient failure), and the `reqwest::Client`
(`anthropic.rs:25`, openai) has no `.timeout()`.
- Honor `Retry-After` when present; retry transport/timeout errors with the same
  backoff; set a sane client `.timeout(...)`.
- **Acceptance:** test that a simulated timeout is retried; that a `Retry-After`
  delay is respected.

### A8. Don't let tool schemas lie to the model
- `tools.rs:289` / `tools.rs:437` — the `tags` filter advertises "ALL of these
  tags" but only `.next()` (the first) is applied. Either AND all tags in
  `SearchFilters`, or change the schema description to single-tag.
- `ingest.rs:1066` — `describe_tool_call` reads `input["query"]` for a tool
  whose schema field is `pattern`, so progress shows `searching "?"`. Read the
  right field (ideally deserialize into the executor's `Input` struct so names
  can't drift).

---

## B. Remaining security hardening (post batch-1)

### B1. OAuth: enforce what the metadata advertises  ★high
`oauth.rs` — redirect-URI validation is a prefix check (`https://evil.com` and
`http://localhost.evil.com` pass), the registered `redirect_uri` is stored
`#[allow(dead_code)]` and never compared at the token endpoint, and dynamic
client registration ignores its input. PKCE *is* correctly enforced.
- Parse redirect URIs, exact-match host against an allowlist / the registered
  value, require a delimiter after `localhost`/`127.0.0.1`, and compare the
  token-time `redirect_uri` to the authorize-time one.
- Or, if multi-client OAuth is out of scope, **document honestly** that this is
  a single-user password + PKCE bridge and drop the RFC 7591 claims.

### B2. Constant-time secret comparison
`oauth.rs` password / client_secret / bearer-token comparisons use `String ==`
(short-circuiting). Use `subtle::ConstantTimeEq` or
`ring::constant_time::verify_slices_are_equal`.

### B3. DoS limits on the HTTP/SSE path
`legacy_sse.rs` takes `body: String` into an `UnboundedSender`, and the session
map is uncapped with lazy cleanup. Add `tower_http`/`DefaultBodyLimit`, cap
concurrent SSE sessions, add idle timeouts, and bound the mpsc channel.

### B4. Don't leak filesystem paths in error responses
`rest.rs:328/331` and similar return raw error strings (including paths) to the
client. Log full error server-side; return a generic message.

---

## C. Architecture & correctness (medium)

- **C1. Dedup the agentic loop.** `agent.rs` and `chat.rs` reimplement nearly the
  same turn loop (~100 lines) and have already drifted. Extract one shared
  turn-handler.
- **C2. RAII guard for `query_only`.** `vault.rs` toggles a global
  `set_query_only(true/false)` PRAGMA around reads; an early return would strand
  it ON. Replace with a `Drop`-resetting guard, and document the
  "`Vault` is externally serialized by `Arc<Mutex<>>`" invariant at the
  `unchecked_transaction` sites.
- **C3. Provenance dedup race.** `git.rs` `pending_ai_writes` is inserted *after*
  the commit and keyed on path alone; a human edit in the same window can be
  misattributed. Insert before `stage_and_commit` and key on (path,
  content-hash), or reconstruct attribution from `git blame` at read time.
  Also make the two `lock().unwrap()` calls poison-tolerant.
- **C4. `LIKE` metacharacter escaping** in `search.rs` `path_prefix` (`%`/`_` in
  a prefix are wildcards today); add `ESCAPE '\'` and escape the input.
- **C5. Parent-directory fsync** after `persist` in `writer.rs::atomic_write`
  (the file is durable but the rename may not be on crash). Closes the small gap
  in the README's "no silent data loss" claim.
- **C6. Watcher lifecycle** (`server lib.rs::start_watcher`) drops the
  `WatchHandle` and never joins the FS-event thread on shutdown.

---

## Notes for whoever picks this up
- The working tree had **uncommitted WIP** when batch 1 was written (`rest.rs`
  untracked, several files modified vs HEAD). Commit that WIP separately before
  layering batch 2 so the history stays legible.
- The pre-existing `git::tests::open_or_init_errors_without_identity` failure is
  **not** from this work — it fails identically on the pre-batch-1 tree.
