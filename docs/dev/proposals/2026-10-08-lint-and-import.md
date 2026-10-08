# Lint and import: keeping a vault from decaying

Date: 2026-10-08
Status: proposal — types for discussion before implementation
Builds on: `2026-10-06-attribution-ledger-refactor.md`

## Problem

Personal note systems tend to follow one arc: a fresh start, a flood of
captured material, folders built ahead of content, a pile of logs, then
abandonment, often followed by a new notebook. The research on personal
information management points at the mechanism: organizing costs effort at
capture time, while its value only shows later, so structure is built before
anyone knows what deserves it (Whittaker & Sidner, CHI 1996; Whittaker et
al., CHI 2011). Models of ADHD predict this weighs harder when the payoff is
delayed (Sonuga-Barke 2002).

Arcana should do the maintenance that people do not: find what has decayed,
propose what to do, and ask the human only for small decisions, a few at a
time. It must never delete, never change the human's words without consent,
and never repeat work it has already done.

## Design principles

1. Capture with zero decisions: anything can land in an inbox; agents file it.
2. Search first; folders are a view agents maintain.
3. Maintenance arrives as one small queue (a handful of items a day).
4. Archive, never delete; every action has an undo.
5. Workers propose; only the human decides; arcana executes.
6. Every operation is idempotent: running it again changes nothing that is
   already done.

## Workers

Narrow, cheap model workers (e.g. Claude Sonnet), each with one prompt and
one kind of finding. Each runs as a named agent (`lint:stubs`) through the
ledger, so anything it eventually writes is attributed and reviewable.

| Worker | Finds | May propose |
|---|---|---|
| `stubs` | empty notes, title-only fragments, very short notes | Archive, Iterate |
| `overlap` | duplicates, near-duplicates, versions of one essay | Merge, Archive |
| `stale` | claims contradicting repos, sources or other notes | Iterate (with source) |
| `structure` | orphans, broken links, single-note folders | Move, Link, Archive |
| `inbox` | captured notes not yet filed | Move |
| `import` | notes in another vault | Import, Merge, Archive |

## Types

Workers return structured output only. The schema below is generated from the
Rust types (`schemars`) and given to the model as its tool definition; output
is parsed strictly (`serde`, unknown fields rejected). A parse failure gets one
retry; a second failure is logged and dropped, never partially applied.

```rust
struct Proposal {
    worker: Worker,
    finding: Finding,
    action: ProposedAction,
    reason: String,             // ≤ 280 chars; shown to the human, never executed
    evidence: Vec<Quote>,       // each text must occur verbatim in its note
    confidence: Confidence,     // Low | Medium | High
}

enum Finding {
    Stub { note: NotePath },
    Overlap { notes: Vec<NotePath> },
    Stale { note: NotePath, claim: String, source: String },
    BrokenLink { note: NotePath, target: String },
    Orphan { note: NotePath },
    Unfiled { note: NotePath },
    External { source: SourceRef },
}

enum ProposedAction {
    Archive { note: NotePath },
    Iterate { note: NotePath, brief: String },
    Merge { from: Vec<NotePath>, into: NotePath },
    Move { note: NotePath, to: NotePath },
    Link { from: NotePath, to: NotePath },
    Import { source: SourceRef, to: NotePath, as_: ImportAs },
}

struct Quote { note: NotePath, text: String }

enum Decision {
    Accept,
    Choose(ProposedAction),          // a different action than proposed
    Dismiss { why: Option<String> }, // reason is shown to future workers
    Snooze { days: u8 },
}
```

## From model text to a change: three gates

```
 worker output ──▶ 1. schema ──▶ 2. facts ──▶ queue ──▶ 3. human ──▶ arcana executes
   (JSON)          parses into     notes exist,         decision      a closed set of
                   Proposal,       quotes verbatim,     (keypress)    deterministic ops
                   closed enums    action allowed
                                   for this worker
```

1. **Schema.** Output must parse into `Proposal`. There is no free-form
   instruction field; the action is one of a closed set.
2. **Facts.** Arcana checks the proposal against the vault: every note exists,
   every evidence quote occurs verbatim in its note (which catches invented
   evidence), the action is one this worker may propose, and the targets are
   valid paths. Failing proposals are logged, not queued.
3. **Human.** The decision is a review keypress. Arcana then executes the chosen
   action itself: Archive and Move are file operations; Link, Merge and Iterate
   become ordinary `EditRequest`s from a drafting agent, subject to the same
   ledger rules as any agent edit (the human's words are only ever suggested
   against).

The model's prose is never executed. It appears only as the one-line reason.

## Idempotence

**Proposals** carry a fingerprint: hash of (worker, finding, content hashes of
the notes involved). A proposal with a fingerprint already queued, accepted or
dismissed is not raised again. A dismissed issue returns only if one of its
notes changes. A queued proposal whose notes changed since it was made is
withdrawn and re-evaluated on the next run.

**Imports** never write to the source vault: it is opened read-only and nothing
is moved, edited or committed there. An import ledger in the target vault,
`.arcana/imports/<source-id>.jsonl`, records one line per source file:

```rust
struct ImportRecord {
    source: SourceRef,        // source vault id + relative path
    source_hash: Sha256,      // content when processed
    outcome: ImportOutcome,   // Imported { to } | MergedInto { note } | Archived { to } | Skipped { why }
    at: Timestamp,
}
```

On a re-run, a source file whose hash matches its record is skipped. A file
changed in the source since its record becomes a "source changed" proposal
rather than being re-imported or re-merged automatically. Exact duplicates
(same content hash) within or across sources are collapsed before any worker
sees them.

## Archive

`archive/` mirrors the vault's layout and is excluded from default search and
from agents' context. Archiving is a move (authorship carried by the ledger)
and is undone by moving back. Nothing is deleted; git keeps history.

## The queue

`arcana review` shows decisions only, oldest first, capped at a small daily
number (configurable, default 5). Each item: the finding, the proposed action,
the reason, the evidence quotes, and keys for accept, choose another action,
dismiss (optionally with a reason), and snooze.

## Order of work

1. Types, schema generation, the three gates, fingerprints, the queue
   (`Proposal`, `Decision`, storage under `.arcana/lint/`).
2. `archive/` with undo; review integration.
3. `stubs` worker end to end on a real vault.
4. Import ledger and the `import` worker; exact-duplicate collapsing.
5. `structure`, `overlap`, `inbox`, `stale`.

## Open decisions

1. Worker model and cost ceiling per run (Sonnet by default; a per-run token
   budget with a dry-run estimate first). Recommend: dry run by default.
2. Daily queue cap. Recommend 5 (leaning).
3. Whether `Iterate` drafts immediately on accept or waits for a second review of
   the draft. Recommend immediately, since agent text lands unreviewed and
   visible like any other agent edit (leaning).
