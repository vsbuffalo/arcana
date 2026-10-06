# Arcana as a co-written textbook: authorship model, note kinds, and write discipline

Date: 2026-10-06 Status: proposal — for discussion Related:
`docs/dev/incidents/2026-10-06-ai-notes-reattributed-to-human.md`

## Problem

The goal is a personal knowledge base that reads like a clean textbook written
for one reader, drafted and refined by agents, in which the human's own words
are visible, protected from being written over, and _provably_ the human's —
line by line, to the standard needed to publish a co-written blog post and say
honestly which words are mine.

The vault today falls short in three ways, and each one traces to a design
choice rather than to carelessness in use.

**Provenance is inferred after the fact, and defaults to human.** Attribution is
whoever made the git commit, and any change arcana did not make itself is
committed as human. Every write path arcana does not own — a Claude Code `Write`
call, a copied repo document, a second arcana process — therefore produces
human-attributed text. The incident report measures the result: 49 AI-created
notes, 11,672 lines, are now blamed on the human. Even without that bug,
`git blame` is line-granular, so it cannot express the case that matters most
for co-writing: a human sentence into which the AI inserted a citation.

**Every document is the same kind of thing.** The vault holds at least five
kinds of document with different owners and lifecycles, and arcana treats them
all as "a note" that any agent may create or overwrite wholesale:

| Kind in the vault today | Example                                                                                        | Owner                    | Lifecycle                   |
| ----------------------- | ---------------------------------------------------------------------------------------------- | ------------------------ | --------------------------- |
| Reference / textbook    | `notes/resonance-and-q-factor.md`                                                              | AI drafts, human curates | refined in place, forever   |
| Human writing           | `blog-posts/*`, `ideas/*`                                                                      | human                    | AI may suggest, not write   |
| Records                 | `component-inventory.md`, bench tests                                                          | either                   | append-only                 |
| Mirrors of repo docs    | `projects/camdl/camdl-language-spec.md` (3,110 lines; the repo copy is 6,347 and has moved on) | the repo                 | should not be copied at all |
| Session dumps           | `notes/2026-04-07-session-summary.md`, audits, status pages                                    | an agent session         | ephemeral                   |

Mirrors and session dumps are the bulk of the volume (the 20 largest notes are
almost all camdl specs, audits and architecture dumps) and none of it is
textbook material. A textbook needs refinement in place; these categories only
ever accrete.

**The messy path is the easy path.** `vault_create` and whole-body
`vault_update` are frictionless (the MCP instructions tell agents to skip
drafts), while the tidy/review pipeline that would keep things in order is a
batch job with an interactive review step, so it is not used. Nothing tells an
agent where a concept already lives, so it writes a new note: the 29 March
74HC595 session written up twice (`notes/electronics/lab-notebook/` and
`projects/stm32-projects/lab-log.md`), sibling `lab-notebook/` and `lab-notes/`
directories, `ramsey/architecture.md` beside `-v2` and `-deep`,
`projects/weather-station` beside `projects/wxstation`, klebsim notes in two
trees. The taxonomy that is supposed to route writes lists
two projects; the vault has twelve project directories under `projects/`, plus
`work-projects/` and `blog-posts/`, which are not zones at all.

## What counts as AI writing

There is no need to invent the line; editors and publishers have drawn it, and
largely in the same place. The shared rule is that a _meaning-preserving_ AI
edit of human text — spelling, grammar, punctuation, readability, formatting —
is not AI authorship, while AI-originated _wording_ or _ideas_ is.

The clearest statements:

- **ACL 2023** (Boyd-Graber, Okazaki & Rogers,
  [ACL 2023 policy on AI writing
  assistance](https://2023.aclweb.org/blog/ACL-2023-policy/), 10 Jan 2023) gives
  a graded ladder: assistance purely with language ("paraphrasing or polishing
  the author's original content") and short-form input assistance need no
  disclosure; literature search needs none provided the authors read and cite
  the sources themselves; low-novelty generated text must be disclosed and
  checked; new ideas must be acknowledged; new ideas plus new text are
  discouraged.
- **Nature Portfolio**, former policy (verified in the Wayback snapshot of
  2025-12-21 of
  [nature.com/…/ai](https://www.nature.com/nature-portfolio/editorial-policies/ai)):
  "'AI assisted copy editing' … AI-assisted improvements to human-generated
  texts for readability and style, and to ensure that the texts are free of
  errors in grammar, spelling, punctuation and tone … may include wording and
  formatting changes … but do not include generative editorial work and
  autonomous content creation" — and needs no declaration. The current page
  replaces this with a three-tier risk framework: language polishing is green
  ("does not introduce new intellectual content", "reversible and verifiable"),
  extensive copy editing is amber, fabricated citations are red.
- **Elsevier**
  ([policy](https://www.elsevier.com/about/policies-and-standards/the-use-of-generative-ai-and-ai-assisted-technologies-in-writing-for-elsevier),
  updated July 2026): "Basic checks of grammar, spelling and punctuation need no
  declaration" — but "if … any AI tool was used to select, collate, generate or
  edit references, this should be noted."
- **Wiley**
  ([AI guidelines](https://www.wiley.com/en-us/publish/article/ai-guidelines/)):
  AI "that creates or transforms content (requiring disclosure) versus AI that
  polishes existing work (typically no disclosure needed)"; formatting citations
  is on the polishing side.
- **IEEE**
  ([author guidelines](https://open.ieee.org/author-guidelines-for-artificial-intelligence-ai-generated-text/)):
  "editing and grammar enhancement … generally outside the intent of the above
  policy."
- **Authors Guild** "Human Authored" certification
  ([announcement](https://authorsguild.org/news/human-authored-certification-expands-to-all-authors/),
  2 Mar 2026): text "fully authored by one or more human beings and not
  generated by AI, except for a de minimis amount (such as through the use of
  AI-powered spelling and grammar check applications)"; AI use for research,
  brainstorming or outlining does not disqualify.
- **US Copyright Office**, _Copyright and Artificial Intelligence, Part 2_
  ([Jan 2025](https://www.copyright.gov/ai/Copyright-and-Artificial-Intelligence-Part-2-Copyrightability-Report.pdf)):
  "assistive uses that enhance human expression do not limit copyright
  protection, uses where an AI system makes expressive choices require further
  analysis"; the test is who "determines the expressive elements"
  ([2023 registration guidance](https://www.copyright.gov/ai/ai_policy_guidance.pdf)).
- **Grammarly Authorship**
  ([support article](https://support.grammarly.com/hc/en-us/articles/29548735595405))
  is the closest existing _tool_: it tracks text as typed, AI-generated, or
  pasted from an unknown source, and keeps typed-then-grammar-corrected text as
  typed text with an edit annotation — the same two-axis structure proposed
  below.

Stricter positions exist and should bound what we claim. Newsrooms set a
_materiality_ threshold rather than a copyedit exemption — the NYT discloses
"substantial use"
([principles](https://www.nytco.com/press/principles-for-using-generative-a%E2%80%A4i%E2%80%A4-in-the-timess-newsroom/),
May 2024), Reuters discloses where AI "is material to the result" (staff memo,
May 2023, read via a reprint). ICMJE and COPE require disclosure of AI use in
writing with no carve-out, and Wired states "We do not publish text edited by AI
either" ([policy](https://www.wired.com/about/generative-ai-policy/)), though
its example is cutting a 1,200-word story to 900 words, a substantive edit.

For machine-readable provenance, **C2PA 2.2**
([spec](https://spec.c2pa.org/specifications/specifications/2.2/specs/C2PA_Specification.html))
distinguishes an _editorial_ transformation ("alters either the intent or
meaning or both") from a non-editorial one, and the **IPTC digital source type**
vocabulary ([cv.iptc.org](https://cv.iptc.org/newscodes/digitalsourcetype/))
names `algorithmicallyEnhanced` ("modification or correction by algorithm
without changing the main content … initiated or configured by a human")
separately from `trainedAlgorithmicMedia` (generated). Neither has text-specific
terms, but C2PA 2.x supports regions within text documents, so span-level claims
fit the standard.

Read together, the sources vary along six axes: who originated the _idea_; who
chose the _words_; whether the edit _preserves meaning_; _how much_ (de minimis,
substantial, material); who _initiated_ it; and whether a human _reviewed_ it.
The design below records the first five per span and treats the sixth as a
property of the publish step.

**Citations need their own class.** Formatting a reference the human chose is
polishing (Wiley). Having the AI _choose_ the reference is disclosable
(Elsevier), and a fabricated reference is the one use Nature now forbids
outright. So `CitationFormat` and `CitationInsert` are different edit kinds, and
`CitationInsert` should carry a verification state (resolved DOI or not).

## Design, in types

### Provenance is recorded per character, and authorship is a declared policy over it

Two layers, deliberately separate. The bottom layer is an objective record: for
every character, who put it there, and in what kind of operation. The top layer
is a _named, versioned policy_ that maps that record to an authorship label per
sentence. Facts are recorded once; what counts as "my writing" is a rule that
can be stated, cited, and re-run.

```rust
/// Who caused a character to exist. Recorded at write time, never inferred.
enum Actor {
    Human,                                  // witnessed human edit (see trust boundary)
    Ai { model: String, session: SessionId },
    Import { source: String },              // pasted / copied from an external document
    Unattributed,                           // arrived by a path arcana did not witness
}

/// What kind of operation an actor performed on existing text.
enum EditKind {
    Compose,     // new prose
    Mechanical,      // spelling, punctuation, whitespace, markup; no content-word change
    CitationFormat,  // formats a reference the human chose
    CitationInsert { verified: bool }, // AI chose the reference; verified = DOI/URL resolved
    Copyedit,        // small, meaning-preserving wording change made at the human's request
}

struct Span { range: Range<usize>, actor: Actor, kind: EditKind, at: Timestamp }
struct Provenance { note: NoteId, base: ContentHash, spans: Vec<Span> }

/// Derived, per sentence, by a named policy.
enum Authorship {
    Human,                                   // human-composed, no AI characters
    HumanAssisted { kinds: Set<EditKind> },  // human-composed; AI did only Mechanical/Citation/Copyedit
    AiDraftHumanEdited,
    Ai,
    Unattributed,
}

trait AuthorshipPolicy {
    const NAME: &'static str;               // e.g. "strict-v1", "light-edit-v1"
    fn classify(&self, sentence: &str, spans: &[Span]) -> Authorship;
}
```

The `Copyedit` bound must be mechanical so that the label is reproducible: for
example, at most _k_ changed content tokens per sentence, no added sentences,
and only when the human requested it. The light-edit policy treats those
sentences as `HumanAssisted`; a strict policy treats any AI character as
disqualifying. A publish check is then a statement with a name in it —
`arcana attest blog-posts/ocaml/post.md --policy light-edit-v1` — and its output
lists every sentence that is not `Human` or `HumanAssisted`.

**Storage.** A sidecar per note (`.arcana/prov/<note-id>.json`, committed to
git) holding the span list against a content hash. On each write, arcana diffs
the previous content against the new one at character level and attributes the
inserted characters to the writer. If the file changed and the sidecar's hash
does not match, the changed characters are `Unattributed` — not human.

Alternative considered: a CRDT (Automerge records an actor per operation and
gives character authorship natively). It is the principled primitive but would
put a document runtime between the editor and plain markdown files.
Recommendation: sidecar plus character diff — _leaning_; revisit if concurrent
human/agent editing of the same note becomes common.

### The trust boundary for "human" is stated, and enforced where possible

A guarantee is only as strong as the claim "every non-arcana write is the
human". Make that claim true rather than assumed:

1. Agents write the vault only through arcana. In Claude Code this is
   enforceable today by denying `Write`/`Edit` on the vault directory in
   settings, leaving the MCP tools as the only write path.
2. Arcana processes stop writing git independently: one writer (the server),
   with a cross-process lock as defence (incident fix).
3. A filesystem change not made through arcana is `Human` only if it occurs
   while the boundary above holds; anything else is `Unattributed`.
4. Later, optionally: an editor-side witness (Neovim extmarks or an Obsidian
   plugin) that reports keystroke-origin edits, which upgrades "human by
   elimination" to "human by observation".

### Human text has priority: agents send operations, not bodies

Whole-body replacement goes away. Agents edit with operations against a base
hash:

```rust
enum EditOp {
    InsertAfter { anchor: SectionId, text: String },
    ReplaceSection { section: SectionId, text: String },
    Annotate { span: Range<usize>, comment: String },        // margin note, never inline
    LightEdit { span: Range<usize>, text: String, kind: EditKind },
}
struct EditRequest { note: NoteId, base: ContentHash, ops: Vec<EditOp>, actor: Actor }
```

The server rejects a request whose `base` is stale (no lost updates over a fresh
human edit), and rejects any op that changes human-attributed characters unless
it is a `LightEdit` within the policy bound on a note where the human has
allowed light edits. Otherwise the change is converted into an `Annotate` — a
suggestion rendered beside the human text. Human words can be _commented on_ by
default, _lightly edited_ by permission, and never _rewritten_.

### Note kinds carry the write rules

```rust
enum NoteKind {
    Chapter,   // textbook: AI drafts and refines in place; one concept per chapter; lives in an outline
    Writing,   // human-owned prose: AI may Annotate, and LightEdit if allowed
    Log,       // append-only dated entries; past entries immutable to agents
    Pointer,   // external source of truth: link + version + short summary, never a copy
}
```

Kind is a frontmatter field, defaulted by zone (`blog-posts/` → `Writing`, lab
notebooks → `Log`). Session summaries are not a kind: an agent session that
wants to persist something writes it into a `Log` or refines a `Chapter`.

### The textbook has an outline, and agents refine before they create

Each subject (`notes/electronics/`, …) has an outline file: an ordered table of
contents with one entry per chapter, and prerequisites. An agent that wants to
record something must first resolve where it belongs — `vault_place(topic)`
returns the chapter and section by search over titles, outline entries and
headings — and the default action is to refine that section. Creating a chapter
requires adding it to the outline, and is refused when a chapter above a
similarity threshold already exists. This is what keeps one write-up of a lab
session instead of two.

## Plan

1. **Incident fix** (small, independent of the rest): commit trees from `HEAD`
   plus named paths, single writer, cross-process lock, relative paths in
   messages; stop adopting unknown changes as human. Land the repro as a
   regression test.
2. **Provenance layer**: `Actor`/`Span` sidecar, character diff on every write,
   `arcana blame` at character level, `arcana attest --policy`. One-off repair
   of the 49 misattributed notes by replaying history.
3. **Write discipline**: `EditRequest` with base hash and human-priority rules
   in the MCP server; deprecate whole-body `vault_update`; deny direct vault
   writes to agents in Claude Code settings.
4. **Note kinds and outlines**, then the refine-first placement tool.
5. **Vault cleanup** using the new tools: mirrors become `Pointer`s, session
   dumps are deleted or folded into logs, duplicates merged.

## Decisions

1. **Incident fix lands ahead of the redesign.** Done in `525331c`.
2. **Provenance is a per-note sidecar, attributed per word** (not per
   character: words are the unit of authorship, and whitespace carries none,
   so re-wrapping changes nothing). A CRDT store is rejected for now: it would
   make plain markdown an export rather than the source.
3. **`light-edit-v1` is the default publishing policy**; `arcana attest`
   reports the `strict-v1` reading alongside it.
4. **Review gate on by default for agent changes**, configurable per note kind
   (`ApplyThenReview` available for chapters). The draft-session machinery is
   replaced by pending changes and suggestions reviewed in a TUI; see
   `2026-10-06-attribution-ledger-refactor.md`.
5. **Existing history is replayed** to recover attribution; anything that
   cannot be traced to a commit is `Unattributed`.
