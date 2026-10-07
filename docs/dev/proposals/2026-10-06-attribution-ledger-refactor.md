# Attribution ledger: one write path, recorded authorship, review-first TUI

Date: 2026-10-06
Status: prototype on branch `ledger` (`0c8f498`, `c3f1bbb`); see "Prototype scope"
Builds on: `2026-10-06-authorship-model.md` (what counts as AI writing).
Review: `docs/dev/reviews/2026-10-06-attribution-ledger-review.md`.

## Goal

Arcana keeps a markdown vault in which agents maintain a personal textbook and
the human keeps their own writing and logs, with the author of every word
recorded at write time. After this refactor:

1. Every word in the vault has a recorded author: the human, a named agent, or
   *unattributed*. Nothing defaults to the human.
2. Only a path that saw the human can credit the human: an `$EDITOR` handoff, a
   review decision, or an edit made outside arcana while agents are barred from
   writing the vault.
3. An agent cannot change the human's words. It can only suggest, and a
   suggestion changes nothing until the human accepts it.
4. A note's content and its attribution are checked against each other on every
   load; a mismatch makes the difference unattributed, never human.
5. Agents refine the textbook in place, driven by the human's questions, rather
   than accreting new notes.
6. Failures are loud: if a commit or attribution write fails, agent writes stop
   and the failure is visible.

Scope for the prototype: text copied in from an outside LLM chat is out of
scope. An edit the human makes outside arcana is the human's.

## Why the current design produces the failures we saw

| Current shape | What it allows | Where |
|---|---|---|
| `ProvenanceAuthor::from_email`: anything not `@arcana.local` is human | unknown writers become human | `git.rs` |
| `commit_human_change` and `commit_ai_write` are both public; the caller picks the author | any caller can claim to be human | `git.rs` |
| REST `author: String`; anything but `"ai"` commits as human; deletes always human | an API client asserts human authorship | `rest.rs` |
| `restore()` commits as human | restoring an AI version credits the human | `git.rs` |
| `NoteWriter::update(body)` replaces the whole body | an agent overwrites human text | `writer.rs`, `agent/tools.rs` |
| Five content-writing entry points plus the watcher | nowhere to enforce anything | `vault.rs`, `drafts.rs` |
| Attribution lives only in git history, by commit author | content and attribution drift apart (49 notes, 11,672 lines) | incident report |
| `vault_create` exists and is pre-approved in the vault's Claude Code settings; server instructions say to skip drafts | agents create notes and session summaries freely | `server/lib.rs`, vault `.claude/` |
| Git failures are logged as warnings only | commits failed for three months unnoticed | `server/lib.rs` |

## The design in one picture

Every writer reaches the vault through one function. What a change is credited
to is decided by *how it arrived*, never by what the caller says:

```
 writers                        authority presented           the one write path
 ───────                        ───────────────────           ──────────────────
 agent (MCP edit) ──▶ plan() ─┬─▶ AgentToken ─────────────────┐
                              └─▶ Suggestion ─▶ review ─┐      │
 you, review TUI / web ──────────────────────▶ HumanWitness ──┤
 you, $EDITOR handoff ───────────────────────▶ HumanWitness ──┼──▶ Vault::commit(change, authority)
 you, Obsidian / nvim ──▶ watcher ──────────▶ Observed ───────┘        │
                          (agents barred from the vault)               ├─▶ note.md          content
                                                                       ├─▶ .arcana/attr/<id>.attr   who wrote each word
                                                                       ├─▶ git commit + trailers    history
                                                                       └─▶ counts file              status line
```

## Types

### Authority is presented once; the recorded author is derived

```rust
/// What a writer presents to `commit`. Taken by value: each is used for exactly
/// one change. Constructors are private to `arcana_core::attr`.
pub enum Authority {
    Human(HumanWitness),     // review decision or $EDITOR handoff
    Observed(Observation),   // watcher saw a file change while agents were barred
    Agent(AgentToken),       // agent id + session + the request text
}

pub struct HumanWitness { via: WitnessPath }          // not Clone, Default, or Deserialize
enum WitnessPath { EditorHandoff, ReviewDecision }

/// The stored author of a run of words. Output only: built inside `commit`
/// from the Authority, serialized to the sidecar, never deserialized into
/// something that can be passed back to `commit`.
pub enum Author {
    Human { via: HumanVia, at: Timestamp },   // HumanVia = EditorHandoff | ReviewDecision | Observed
    Agent { id: AgentId, at: Timestamp },
    Unattributed { at: Timestamp },
}
```

Who can mint what:

```
  inside arcana_core::attr                     outside (agent, server, REST)
  ────────────────────────                     ─────────────────────────────
  review decision ─┐                           ✗ cannot construct HumanWitness
  $EDITOR handoff ─┴─▶ HumanWitness ─┐         ✗ cannot construct Observation
  watcher* ──────────▶ Observation ──┼─▶ commit() ──▶ Author (stored)
  MCP edit ──────────▶ AgentToken ───┘         ✓ can request an AgentToken for its own session
                                               ✓ can read Author; cannot turn it back into Authority
  * only while the write boundary holds (below); otherwise the change is Unattributed
```

Why this shape: authority and the stored label are different things. Reading a
note's attribution (blame, the MCP `read` tool) yields `Author` values, which
`commit` does not accept, so attribution cannot be copied from one place onto
another. Each `HumanWitness` is consumed by one decision, so one keypress cannot
approve a batch it did not cover.

**What the type system does and does not guarantee.** It guarantees that no
code outside `attr` can produce human authority and that each authority is used
once. Whether an agent's edit touches only agent-owned words is a fact about
the data, so it is checked at run time, inside `commit`, against the current
sidecar. That check lives in exactly one function.

### Attributed text: words, runs, and an integrity hash

```rust
/// Content plus an author for every word. Fields private; built only by
/// `Ledger::load` or by `commit`.
pub struct AttributedText { content: String, runs: Vec<Run> }

struct Run {
    span: ByteRange,        // in `content`
    author: Author,
    origin: Origin,
    policy: Option<PolicyId>,   // e.g. light-edit@1 on an accepted light edit
}

pub enum Origin { Composed, Assist(AssistKind), CitationInsert }
pub enum AssistKind { Mechanical, CitationFormat, Copyedit }

/// The sidecar, `.arcana/attr/<note-id>.attr`, committed with the content.
struct Sidecar { note: NoteId, tokenizer: TokenizerVersion, tokens_hash: Sha256, bytes_hash: Sha256, authors: Vec<AuthorEntry>, runs: Vec<Run> }
```

**Words carry authorship; whitespace does not.** The tokenizer splits text into
words, punctuation and markdown markup; whitespace is a separator. In
CommonMark a newline inside a paragraph is the same as a space, so re-wrapping
(`gq` in Neovim, Obsidian's wrapping) changes no tokens and no authorship:

```
 before gq:  "Resonance is the response of any linear second-order system."
 after gq:   "Resonance is the response of any\nlinear second-order system."
 tokens:     Resonance│is│the│response│of│any│linear│second│-│order│system│.
             identical sequence → identical runs → no change in authorship
```

Exceptions where whitespace is content: fenced code (tokenized by line), hard
breaks, and tables (by cell). Inline math `$…$` is one token. The tokenizer
version is stored in the sidecar so a tokenizer change can never silently
re-align old runs.

**The sidecar checks itself.** It stores the hash of the content it describes
(borrowed from iA Writer's Markdown Annotations format, which stores a SHA-256
of the annotated text and requires tools to warn on mismatch). On load, if the
note's content does not match, arcana diffs the last committed content against
the file and attributes the difference by the rules for an outside edit: the
human's if the write boundary holds, unattributed otherwise. This also makes
`git checkout`, `revert` and merges safe, because the comparison is always
between the file and its own committed sidecar.

### The sidecar format: Markdown Annotations, adapted

The on-disk format starts from iA Writer's Markdown Annotations v0.2
(github.com/iainc/Markdown-Annotations): an author table with sigils (`@`
human, `&` AI), ranges, and a hash of the annotated text. No Rust library
exists for it, so arcana writes its own, and changes it where the original's
design conflicts with ours:

| Markdown Annotations v0.2 | Arcana | Why |
|---|---|---|
| Block appended to the end of the `.md` file | Separate file, `.arcana/attr/<note-id>.attr` | Notes stay clean; content diffs are not interleaved with attribution diffs |
| Ranges in grapheme clusters over the raw text | Ranges in word tokens | Whitespace carries no authorship, so re-wrapping changes no range |
| One hash over the raw text; any re-wrap invalidates every annotation | Two hashes: the token stream (attribution validity) and the bytes (file identity) | A re-wrap changes the byte hash only; the attribution stays valid |
| On mismatch, warn; the user keeps or discards the annotations | On mismatch, re-diff against the last committed content and carry runs forward | The previous text is in git, so reconciliation is automatic rather than a prompt |
| Author is a name only | Each author entry records kind, agent and session, request; each run records origin and policy | Needed for `light-edit@1` and for review |

The file is line-oriented, one run per line, so a git diff of a sidecar is
readable:

```
arcana-attr 1
note 0b6f2c1e  tokenizer tok-1
tokens sha256:5e1a…  bytes sha256:c903…
@v    human  vince
&a1   agent  claude-opus-5-5  session 91ab  "add the bandwidth derivation"
0,212    @v   composed
212,9    &a1  citation-insert
221,40   @v   composed  assist:copyedit  light-edit@1
261,88   &a1  composed  unreviewed
```

Export to the original Markdown Annotations format (converting token ranges
back to grapheme ranges) stays on the deferred list, so iA Writer can display
arcana's attribution.

### Notes have stable identity

```rust
pub struct NoteId(Uuid);   // stored in the sidecar; path → id map in the index
```

A note renamed in Obsidian arrives at the watcher as a deletion plus a new
file. The watcher matches the new file to the orphaned sidecar by content hash
(or high similarity) and records a `Rename`, carrying every run. Without this,
renaming a note would make every word in it the human's.

### Diffing: words, then moves

Each save is diffed in two passes:

1. A histogram diff over word tokens, snapped to word boundaries
   (diff-match-patch's lossless semantic cleanup).
2. A move pass: deleted and inserted runs of at least 8 tokens within the same
   change are matched by hash, and matched text keeps its original author.

So cutting an agent paragraph and pasting it lower down in Neovim leaves the
agent as its author. Cross-note moves are handled by the structural `Move` op
when arcana does them; a hand cut-and-paste between two notes is attributed as
an ordinary edit (prototype scope).

### Agents edit with requests; the planner decides what they may touch

```rust
pub struct EditRequest {
    note: NoteId,
    base: Sha256,                 // the content the agent read
    rationale: String,            // one line, shown in review
    answers: Option<QuestionId>,
    edits: Vec<RawEdit>,          // anchored by block id + quoted anchor text
}

pub struct Plan { changes: Vec<PlannedEdit>, refused: Vec<(RawEdit, Refusal)> }
pub enum PlannedEdit {
    Agent(Edit),           // touches only agent-owned words
    Suggest(Suggestion),   // touches, or inserts into, a human-owned block
}

pub fn plan(text: &AttributedText, kind: NoteKind, req: EditRequest) -> Plan;
```

Rules the planner applies:

- **Insertion counts as touching.** An insertion between two human words, or
  anywhere inside a human-owned block, is a suggestion. Ownership of the gaps
  between words belongs to the enclosing block (paragraph, list item, cell).
- **New text in a chapter is not gated**, but it is marked unreviewed and
  appears in the review queue.
- **Plans carry their base.** Every `Edit` and `Suggestion` records `base` and
  its anchor text. `commit` re-checks ownership against the current sidecar; if
  the note changed, the original `RawEdit` is planned again from scratch.
- **Light edits are bounded mechanically.** A suggestion qualifies as
  `Assist` under `light-edit@1` only if it is one of the listed kinds and
  changes at most a fixed number of tokens per block (initially 3 for
  `Copyedit`, any number for `Mechanical` and `CitationFormat`, one citation per
  `CitationInsert`). Anything larger is an ordinary suggestion, and once
  accepted those words belong to the agent. The accepted run records the policy
  that admitted it.

What happens to a request:

```
 EditRequest{base} ──▶ base current? ──no──▶ re-plan from RawEdits (or refuse if anchors are gone)
                            │yes
                            ▼
              for each edit: whose block does it touch?
        ┌───────────────────┼─────────────────────────┐
  agent-owned only     human-owned block        forbidden by note kind
        │                   │                         │
        ▼                   ▼                         ▼
   Edit (gated or      Suggestion ──▶ review      Refused, with reason
   applied, by kind)          │
        │            accept: HumanWitness ──▶ commit (you remain author; light-edit recorded)
        ▼            reject: recorded, nothing written
      commit
```

### One write path

```rust
impl Vault {
    /// The only function that writes note content.
    pub fn commit(&self, change: Change, auth: Authority) -> Result<Committed, CommitError>;
}

pub struct Change { note: NoteId, base: Sha256, ops: Vec<Op>, request: Option<String>, group: Option<GroupId> }
pub enum Op { Replace(Edit), Rename(Path), Move { to: NoteId, at: BlockId }, Delete, Create(NoteKind) }
```

`commit`, under a cross-process lock:

1. checks `base` against the file on disk;
2. checks the authority permits every op (the planner's rules, re-run);
3. writes a journal entry `(note, new_hash, change_id)`, so the watcher
   recognises the coming file change as this change rather than an outside edit;
4. writes content and sidecar (each atomically), then one git commit built
   from HEAD plus exactly these paths, authored from the `Authority`, with
   trailers:

```
arcana: refine notes/electronics/q-factor.md §Series resonance

Arcana-Change: 7f3c…
Arcana-Author: agent claude-opus-5-5/session-91ab
Arcana-Request: add the bandwidth derivation, cite French 1971
```

5. records undo information: the runs each op replaced. Undo, `restore` and
   `revert` re-install those runs and never re-attribute.

Any error at steps 3–4 sets a sticky `WriteHealth::Failing`, which refuses
further agent changes and appears in the status line until resolved.

Git remains the history store and stops being the source of attribution.

### The write boundary

"An edit outside arcana is the human's" holds only while agents cannot write
the vault directly. The boundary is the operating system sandbox, not a tool
permission: Claude Code's sandbox `denyWrite` on the vault directory, which
also covers `Bash`. `arcana doctor` checks it; the watcher re-checks it on
start and on every config change; while it does not hold, outside edits are
`Unattributed`. The vault's current pre-approval of `vault_create` and
`vault_update` is removed.

### Note kinds and permissions

```rust
pub enum NoteKind { Chapter, Writing, Log, Pointer(SourceRef) }
pub struct SourceRef { repo: PathBuf, path: PathBuf, rev: GitRev }
```

The kind is set at creation and changed only with a `HumanWitness`, so an agent
cannot relabel your writing as a chapter to gain write access.

```
                       Chapter                 Writing        Log               Pointer
 agent, new words      applied, "unreviewed"   suggest        append            regenerate summary
 agent, its own words  gated                   gated          ✗                 ✓
 agent, your words     suggest                 suggest        ✗                 suggest
 you                   ✓                       ✓              append; edit ✓    ✓
 outside edit          yours if the boundary holds, otherwise unattributed
```

Review mode is per kind and configurable. The default gates every agent change
to existing text and every suggestion to your words; new agent text in a
chapter (a new section, an answer to your question) is applied immediately and
marked *unreviewed* until you review it, so the textbook grows without a queue
of whole-chapter drafts that never get read. A `Pointer` note is a short summary plus a link to a repo document at a
pinned revision; `arcana stale` reports pointers whose source has commits since
that revision.

### Questions and the reader model

```rust
pub struct Question { id: QuestionId, at: BlockId, note: NoteId, text: String, asked: Timestamp, state: QState }
pub enum QState { Open, Answered(ChangeId), Closed }
```

- A question is anchored to a block and stored in the note's sidecar, not
  inline in the prose.
- Questions are created from three places (see below), always as the human's.
- Agents see open questions in `read` and answer with an `EditRequest` whose
  `answers` names the question. Review shows the question above the diff;
  accepting the answer marks it `Answered`, and you close it when it actually
  landed.
- `reader.md` (Writing kind, yours) states your background, and each subject's
  outline marks concepts `known | shaky | new`. `read` and `place` return the
  relevant parts so agents pitch chapters at your level.

## The MCP surface: five tools

| Tool | Does |
|---|---|
| `search(query, filters)` | full-text search |
| `read(note)` | content, kind, outline position, per-section ownership, open questions, and pending changes (so a new session does not re-propose them) |
| `place(topic)` | ranked candidate chapters and sections for a topic |
| `edit(EditRequest)` | plans the request and returns the plan with its word diff; creating a chapter is an op that must name the candidates it rejected and why |
| `log(target, entry)` | appends to a Log note (default: today's project log, or the inbox); refuses entries over ~300 words with "refine a chapter instead" |

Removed: `vault_create`, `vault_update`, `vault_draft`, `vault_suggest_edit`,
`vault_list`, `vault_stats`, `vault_provenance`. With no `create`, there is
nowhere for a session summary to go. The server instructions are rewritten
around refine-before-create.

## The review TUI

The middle layer is a `ReviewSession`; the TUI (`arcana-tui`, ratatui) is the
first projection of it, and a small web view served on the Tailscale address is
the second.

```rust
pub struct ReviewSession { queue: Vec<ReviewGroup> }      // grouped by request, then section
pub struct ReviewGroup { request: String, summary: String, items: Vec<ReviewItem> }
pub enum ReviewItem { Edit(PlannedChange), Suggestion(Suggestion), Question(Question) }
pub enum Decision { Accept, Reject { reason: Option<String> }, EditMyself, Skip }
```

```
┌ queue ─────────────────────┐┌ notes/electronics/q-factor.md § Series resonance ─────────┐
│▸ add bandwidth derivation 3││ request: add the bandwidth derivation, cite French 1971    │
│   q-factor §Series      ●  ││ why:     derivation answers your question below            │
│   q-factor §Bandwidth   ○  ││ answers: "why is Δω = ω₀/Q and not 2ω₀/Q?"                 │
│  fix typos (blog) 5      ││                                                            │
│  pointer refresh 2         ││ The bandwidth is [-the width-]{+the full width at half     │
│                            ││ power+} of the resonance, Δω = ω₀/Q [@french1971].         │
│                            ││                                                            │
└────────────────────────────┘└ a accept · r reject · c reject+why · e edit · ? ask · q ─────┘
```

- Opens straight on the first undecided item; every decision advances.
- Inline word diff (tmux panes and phones are narrow); moved text in its own
  colour; `t` cycles final / original / marked-up; `o` toggles colouring by
  author.
- Keys: `a` accept, `r` reject, `c` reject with a reason that returns to the
  agent, `e` edit in `$EDITOR`, `?` ask a question about this section, `s`
  split, `.` repeat last decision, `A`/`R` accept or reject the rest of the
  group (agent-owned edits only; suggestions to your words are always decided
  one at a time), `J`/`K` next/previous note, `u` undo, `q` quit with a tally.
- Review state persists across sessions; items older than a week sort first;
  section edits over ~150 changed tokens are split by the planner.
- Pending counts are written to a small file for tmux `status-right`
  (`✎7 ?2 ⚠git`) and a Claude Code `SessionStart` hook line.
- The web view's decisions are bound to a credential agents do not hold, and
  the review endpoints are never on the MCP router. An "accept" relayed by an
  agent is not a human decision.

## Where questions come from

- **In review:** `?` on the section you are looking at.
- **In Neovim or Obsidian, while reading:** a `CC?` line under the paragraph,
  the input-mark convention the `ai-pass` skill already uses. The watcher turns
  it into a `Question` anchored to that block and removes the mark from the
  prose.
- **In conversation with an agent:** "I don't get why parallel R_p looks
  inverted" → the agent calls `ask(note, block, text)`. This is a sixth MCP
  tool, harmless because questions are not prose that gets published.

## Prototype scope

Built: tokenizer, word attribution with move detection, sidecars, the ledger
write path with lock and pending queue, note kinds and types, the six MCP
tools, watcher attribution with rename detection, the review TUI (accept,
reject, reject with reason, edit in `$EDITOR`), word-level `arcana blame`,
and `arcana ledger init | import | status`.

Differs from the design above: sidecars mirror the note path
(`.arcana/attr/<path>.attr`, with the note's UUID inside) rather than being
named by id, which makes rename handling a file move.

Not yet built: questions and the reader model, `place`, `arcana attest`,
undo of review decisions, the web review view, `arcana doctor`'s check of the
write boundary, and the history replay for existing vaults.

## Order of work

0. **Move the vault out of iCloud-synced `~/Documents`** to `~/vault/arcana`.
1. **Triage.** For each existing note, one disposition: keep as chapter,
   writing or log; replace with a pointer to its repo source; delete; or mark
   for a later merge. Dispositions that move no words between notes, so
   nothing can be misattributed. Applied as git deletions and pointer rewrites,
   in batches by directory. Expected ~168 → ~90 notes.
2. **Ledger core** (`arcana_core::attr`): tokenizer, two-pass diff, sidecar
   with hash, `NoteId`, `Authority`/`Author`, `Vault::commit` with journal and
   lock, write health. Bootstrap surviving notes by replaying git history (this
   repairs the misattributed and rolled-back files); anything untraceable is
   unattributed. Deliverable: `arcana blame` at word level.
3. **Planner and MCP surface**: `EditRequest → Plan`, note kinds, light-edit
   policy, the five tools plus `ask`, the sandbox write boundary and
   `arcana doctor`. Deletes the old write paths and the drafts machinery.
4. **Review TUI**, then the web view (3b) for review from the phone.
5. **Questions and the reader model**; `arcana attest` for blog posts.
6. **Merges and moves** of the remaining notes, through `Move` ops, reviewed in
   the TUI.

## Deferred

- Signing arcana's commits so a forged sidecar is detectable (the sandbox
  boundary covers the realistic threat for now).
- Text pasted from an outside LLM (`Import`, paste detection).
- Freshness beyond pointer revision checks; review-date staleness.
- A SQLite cache of attribution (sidecars are fast enough at ~100 notes).
- Export to Markdown Annotations, so iA Writer and the Obsidian "Authorship"
  plugin can display arcana's attribution.

## Decisions

1. Attribution truth is the per-note sidecar, per word, with a content hash.
2. The review unit is the section; edits to your words are decided one at a
   time.
3. Agent text (new or edited) is written immediately and marked
   unreviewed; only changes touching your words wait for you. Holding agent
   edits for review is a switch (`[ledger] review_agent_edits = true`).
4. `light-edit@1` is the publishing policy; `attest` also reports the strict
   reading.
5. An edit outside arcana is yours while the sandbox boundary holds.
6. Commit signing is deferred.
7. Triage before the ledger; merges and moves after it.
