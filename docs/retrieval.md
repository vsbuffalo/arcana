# Retrieval & indexing

How Arcana turns a directory of markdown files into a fast, queryable index, and
how search ranks results. The retrieval layer lives in `arcana-core`
(`src/index/`, `src/search.rs`) and has zero UI or LLM dependencies.

## Data model

A single SQLite database (`.arcana/index.db`, WAL mode) holds four tables:

- **`notes`** — one row per markdown file: `path` (unique), `title`, `body`,
  a denormalized `tags` string, a `content_hash`, frontmatter, AI-provenance
  columns (`is_ai`, `ai_model`, …), and timestamps.
- **`notes_fts`** — an FTS5 **external-content** virtual table over
  `(title, body, tags)`, keyed to `notes.id`. External-content
  (`content='notes'`) means the FTS table stores only its inverted index, not a
  second copy of every note body. Three triggers (`AFTER INSERT/UPDATE/DELETE`
  on `notes`) keep it in sync, so writers never touch FTS directly.
- **`tags`** — normalized `(note_id, tag)` for exact tag filtering, indexed on
  `tag`.
- **`links`** — the wikilink graph `(source_id, target)`, indexed on `target`.

The FTS5 tokenizer is `porter unicode61 remove_diacritics 2`: Unicode-aware word
splitting, diacritic folding (`café` = `cafe`), and Porter stemming (so
"indexing" matches "index").

## Indexing pipeline

`arcana index` is incremental and parallel:

1. **Walk** the vault (`walkdir`, respecting excludes) to collect `.md` paths.
2. **Parse + hash in parallel** (`rayon`): each file is read, parsed
   (frontmatter, body, inline `#tags`, `[[wikilinks]]`), and hashed (xxh3-64) —
   all with *no* database access, so this stage is embarrassingly parallel.
3. **Reconcile in one transaction** (single-threaded): for each parsed file,
   compare its `content_hash` against the stored hash — **skip if unchanged**,
   otherwise upsert. Then prune rows whose files are gone (via a temp table, to
   stay under SQLite's 999-bound-variable limit).

Re-indexing an unchanged vault is therefore cheap: it hashes files and does
near-zero writes. The file watcher behind `arcana serve` reindexes changed paths
live on filesystem events.

Tradeoff worth naming: `content_hash` is a non-cryptographic 64-bit hash, so a
collision would silently skip a genuinely-changed note. At 64 bits over a
personal vault this is negligible, but it is a *silent*-staleness failure mode,
so it's a deliberate speed/robustness trade (a cheap mtime tiebreaker would
close it).

## Query path

`arcana search "<query>"` (and `vault_search` over MCP):

```sql
SELECT n.id, n.path, n.title,
       snippet(notes_fts, 1, '<mark>', '</mark>', '...', 32) AS snippet,
       bm25(notes_fts, 5.0, 1.0, 2.0)                        AS score
FROM notes_fts JOIN notes n ON n.id = notes_fts.rowid
WHERE notes_fts MATCH ?1
ORDER BY score
LIMIT ?2;
```

Optional filters (tag, path-prefix, AI-only) compose as additional `JOIN`/`WHERE`
clauses; snippets are highlighted from the `body` column.

### Ranking

BM25 with per-column weights `(title 5.0, body 1.0, tags 2.0)`: a query term in a
note's **title** counts ~5× a body match, and a **tag** match ~2×, because the
title and curated tags are stronger relevance signals than an incidental body
mention. (SQLite's `bm25()` returns *negative* scores — more relevant is more
negative — so results sort ascending.) The integration tests assert this on the
fixture vault: a note with "SQLite" in its title ranks above one that only
mentions it in the body.

### Query safety — two layers

Query text (from a human or a model) is **both** bound as a parameter **and**
sanitized:

- **Binding** (`?1`) defeats SQL injection.
- **Sanitizing** defeats the problem binding *doesn't*: a bound string is still
  parsed as an FTS5 *query*, so raw input like `a OR (b` is a syntax error or an
  unintended operator. `sanitize_fts_query` strips FTS5 reserved characters
  (`* " ( ) +`) and operator words (`AND/OR/NOT/NEAR`), splits on whitespace,
  and quotes each remaining term as a literal phrase joined by implicit AND. So
  `self-attention` becomes `"self" "attention"`, and `NOT this` becomes
  `"this"` — always a valid, literal query.

Parameterizing *and* sanitizing the operator surface is the part that's easy to
get wrong: binding alone is necessary but not sufficient when the bound value is
itself a query in another grammar.

## Design decisions

- **FTS5/BM25, not an external search service or embeddings.** For a single-user
  markdown vault, embedded lexical retrieval is the right default: zero
  infrastructure, deterministic, fast, and strong exact-term recall — and it
  lives in the same SQLite file as the metadata. No service to run, no model to
  host, no index to keep warm.
- **Lexical, not (yet) semantic.** The honest tradeoff: lexical search misses
  paraphrase ("car" ⇏ "automobile"). Hybrid retrieval (FTS5 + vector embeddings
  fused with reciprocal-rank fusion) is the natural next step, and the schema is
  ready for it — add an embeddings table and fuse at query time. Lexical-first is
  a deliberate "make the cheap, debuggable thing excellent before adding a
  model."
- **External-content FTS5** avoids storing note bodies twice.
- **Reads run under a `query_only` connection PRAGMA**, and the whole `Vault` is
  serialized behind one mutex — simple and correct for a single-user service.

## Limitations

- No semantic/vector search yet (lexical only).
- `path_prefix` filters use SQL `LIKE` without escaping `%`/`_`, so those
  characters in a prefix act as wildcards.
- A single SQLite connection — fine for one user, not built for high write
  concurrency.
