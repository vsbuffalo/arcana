# Review of the attribution-ledger proposals

Date: 2026-10-06
Reviewed: `docs/dev/proposals/2026-10-06-authorship-model.md`,
`docs/dev/proposals/2026-10-06-attribution-ledger-refactor.md`
Reviews: three independent passes — an adversarial soundness review of the
types and protocol, a product/UX review against the vault as it is actually
used, and a prior-art survey of comparable tools. None edited files. This
document records what they found and what we take from it.

## Verdict

The direction holds: one write path, recorded rather than inferred
attribution, `Unattributed` as the default, and human-priority edits are what
all three reviews would keep. Three things are wrong or missing.

1. **The proposal does not detect moves or renames, and without that the
   guarantee is false.** Cutting an agent paragraph and pasting it elsewhere in
   Neovim is a delete plus an insert, so the inserted words are credited to the
   human. Renaming a note in Obsidian arrives as delete plus create (the
   watcher has no rename event, verified in `watcher.rs`), so the whole note is
   credited to the human — the incident's failure again, at word level.
2. **"Human" is claimed more strongly than any path can witness.** The watcher
   sees that a file changed, not who changed it; the sidecar loader trusts a
   file anything with vault write access can forge; and the witness as
   sketched is borrowed, so one review keypress could authorize any number of
   changes.
3. **The proposal serves the attestation goal and barely touches the
   "textbook for me" goal**, which is where the agent writes — and hence the
   mess — come from. There is no reader model, no question-driven refinement
   loop, and the MCP tool surface (including `vault_create`) stays wide enough
   to keep producing new notes and session summaries.

## Findings the reviews agree on

### Move and rename detection are required

Walk the failure: you cut an agent-written paragraph in Neovim and paste it
lower down → the watcher's token diff sees a deletion and an insertion → the
insertion's actor is the watcher's, so the agent's words become yours. The
same happens for a paste from a claude.ai chat and for text written into the
buffer by an editor AI plugin.

The prior-art survey reaches the same conclusion from the other side: `git
diff --color-moved` and Cursor's Agent Trace spec (content hash per span,
"allowing tracking even when code moves") exist because a plain diff cannot
tell a move from a rewrite. Automerge avoids the problem only by giving every
character a permanent identity, which plain files cannot have.

Fix:
- A two-pass diff: histogram diff over word tokens, snapped to word and
  sentence boundaries (diff-match-patch's lossless semantic cleanup), then a
  move pass that matches deleted and inserted runs of at least ~8 tokens by
  hash and *carries their attribution*. Matching also runs against recent
  deletions in other notes (a cut in A and a paste into B arrive in different
  save windows) and against a fingerprint index of agent-owned text.
- A stable `NoteId` with a path map, so a rename is matched to its orphaned
  sidecar by content hash and recorded as `Rename`.
- Moves render in review as a third colour, never as delete plus insert.

### The sidecar needs an integrity hash, and a mismatch means "unknown"

iA Writer's Markdown Annotations format (iA Inc., spec v0.2,
github.com/iainc/Markdown-Annotations) stores a SHA-256 of the annotated text
and requires tools to warn when it does not match. Adopt that: the sidecar
records the hash of the token stream it describes; on mismatch, arcana runs a
reconciliation diff that attributes the difference to `Unattributed`, never to
the human. The same check makes `git checkout`, `revert` and merges safe: the
watcher compares disk content to the sidecar *on disk*, so a branch switch is
not read as a human edit.

The soundness review goes further: anything that can write the vault can write
content plus a matching sidecar, so the hash proves consistency, not origin.
Its fix is signing arcana's commits with a key outside every agent's sandbox.
Recommendation: hash now; defer signing until the OS-level write boundary
(below) is in place and shown insufficient — *leaning*.

### The light-edit policy must be bounded mechanically

Grammarly's Authorship report showed AI text run through its "Humanize" tool
as "75% Typed by a Human" (J. Bailey, Plagiarism Today, 6 Nov 2025); Grammarly
then relabelled AI-rephrased text (Bailey, 11 Dec 2025). `light-edit-v1` is the
same kind of rule and needs the same guard: a whitelist of edit kinds, a cap on
changed tokens per edit and per block, and the policy name and version stored
on the affected runs, so a run reads "human, under `light-edit-v1`" rather than
plain "human".

Sentence segmentation is not well defined in markdown ("et al.", headings,
list items, math), so the bound is per *leaf block* (paragraph, list item,
table cell), with the tokenizer version (`tok-v1`) recorded in the sidecar.

### Agent proposals are named, explained change groups

Ink & Switch's Patchwork notebook found that grouping edits by intent rather
than by time (#05, 29 Feb 2024), bots that leave a rationale with each edit
(#07, 19 Mar 2024), and LLM-written change summaries (#09, 28 Mar 2024 —
"more successful than any of our other diff visualizations so far") made
review tractable. The product review independently said the same: a review
item without the request and the agent's reason is slow to judge. So every
`Change` carries the request, a one-line rationale per hunk, and a group
summary; the TUI groups by agent request first, section second.

## Changes to the type design

These tighten the design and, in two places, simplify it.

| Finding | Change |
|---|---|
| `accept(self, w: &HumanWitness)` borrows the witness; one keypress could authorize many changes | Witnesses are taken by value, one per decision |
| `Run: Clone` makes `Actor::Human(Witnessed)` copyable out of any ledger read | Split *authority* from the stored label: `Vault::commit(change, auth: Authority)` with `Authority = Human(HumanWitness) \| Agent(AgentToken) \| Observed(Channel)`, by value. `Actor` becomes output only, derived inside `commit`, `Serialize` but not `Deserialize` |
| The ledger loader is a second mint for `Witnessed` | Loading goes through a private DTO inside `attr`; loaded runs are data, not authority |
| `Op<AgentOwned>` does not record the note version it was planned against; re-planning by shifting ranges can make it overwrite words that became human | `Op` and `Suggestion` carry `base` and anchor text; `apply` re-checks ownership; re-planning re-runs `plan` from the original request |
| An insertion between two human words touches no human token, so it is "agent-only" | The gap is owned by the enclosing leaf block: inserting into a human block is a suggestion |
| One section-level `a` accepts agent ops and edits to human words together | Section accept applies to agent-owned ops only; each edit to human words is accepted individually |
| `restore()` commits as human (verified, `git.rs`), so restoring an AI version credits the human | Each `Change` stores the runs it replaced; undo, restore and revert re-install them |
| The watcher's witness is a guess dressed as an observation | Rename it `Observed` and say what it is: "changed by a path other than an agent, while agents were barred from writing". See decision 2 |

On "illegal states unrepresentable": the honest boundary is that *ownership is
a fact about data*, so the check that an edit touches only agent-owned words is
a runtime check inside `commit`, wherever it lives. The phantom type
`Op<AgentOwned>` adds ceremony without adding protection. What the type system
does guarantee is that nothing outside `attr` can produce human authority, and
that each authority is used once. Recommendation: drop the `Op<O>` typestate,
keep `Authority` by value — *leaning*.

## Product additions

### The textbook loop: questions and a reader model

- **Questions drive refinement.** You press `?` on a section in the TUI (or
  ask an agent, or note it in Neovim): "I don't get why parallel R_p looks
  inverted". It becomes a `Question` anchored to that section. Agents see open
  questions in `read` output and answer with an edit that carries
  `answers: QuestionId`; review shows the question above the diff. Open
  questions per chapter are a better signal of where the textbook is weak than
  any date.
- **A reader model.** A human-owned `reader.md` (your background: scientist,
  strong probability and statistics, ODEs, electronics beginner, not an
  epidemiologist) and per-subject outlines that mark each chapter's concepts
  `known | shaky | new`, so an agent writing about Q factor knows you already
  understand damped oscillators and does not re-teach them.

### Five MCP tools, and no `create`

`search`, `read` (note, kind, outline position, per-section ownership, and
*pending changes* so the next agent session does not re-propose them),
`place(topic)` (ranked candidate chapters), `edit(EditRequest)` (returns the
plan and its word diff; creating a chapter is an op that must name the
candidates it rejected and why), and `log(target, entry)` (append-only,
rejects entries over ~300 words with "refine a chapter instead"). With no
`create`, there is nowhere for a session summary to go. The server
instructions, which currently tell agents to skip drafts, are rewritten.

### Failures are loud

Git commits failed for three months with only log warnings. Any failure in the
commit or attribution path blocks further agent writes and shows in a tmux
status count (`✎7 ?2 ⚠git`), read from a small counts file the server keeps
current.

### Capture, logs, and dictated text

Quick capture from the phone and Obsidian daily notes (two already sit at the
vault root) go to a `log(inbox)`. Measurements go to the project's dated log
as a verbatim line plus a structured row. The 825-line component inventory
becomes a data file that is queried rather than prose that is rewritten. Text
you dictate through an agent was relayed, not witnessed: it is
`Actor::Relayed { agent }`, harmless in a log and visible if it ever reaches a
blog post.

### Publishing

`arcana attest <post>` writes `attestation.json` (policy and version, ledger
and content hashes, per-block class) and a short disclosure paragraph ("94% of
blocks human; 6 human-assisted — citation formatting, 3 typo fixes; 0
AI-composed; strict reading: …"). Annotations render to the existing `ai-pass`
convention rather than a second format. Optional later: export to Markdown
Annotations, so iA Writer and the Obsidian "Authorship" plugin (rflpazini,
v0.1.0) display arcana's provenance.

## The review TUI

From `git add -p`, Word's track changes, tuicr (agavra) and the Patchwork
notebook:

- `arcana review` opens straight on the first undecided item; every decision
  advances (Word's "Accept and Move to Next").
- Each item shows one header line: the request, the agent's reason, and the
  question it answers.
- Inline word diff (tmux panes and the phone are narrow); moves in a third
  colour; owner colouring toggled with `o`; final / original / markup views
  with `t`.
- Keys: `a` accept, `r` reject, `c` reject with a reason that goes back to the
  agent, `e` edit in `$EDITOR`, `?` attach a question, `s` split a hunk, `.`
  repeat last decision, `A`/`R` accept/reject the rest of the group, `J`/`K`
  next/previous note, `u` undo, `q` quit with a tally.
- Review state persists across sessions (tuicr); items older than seven days
  come first; changes whose base moved are re-planned automatically.
- Large section edits (over ~150 changed tokens) are split by the planner or
  parked as "read later".
- **Phone review has to be the web view, not an agent relay.** "Accept" relayed
  by Claude from the phone is not a witnessed human decision. The web view
  therefore moves up to phase 3b, and its decisions are bound to a credential
  agents do not hold (a passkey); the review endpoints never appear on the MCP
  router, which claude.ai can reach.

## Defer or cut

- `Freshness` and `Staleness`, except checking `Pointer` notes against their
  source repo's revision. Open questions replace review-date staleness for
  chapters.
- The SQLite attribution cache: at ~100 notes, reading sidecars is fast enough.
- The `Origin` taxonomy shrinks to `Composed`, `Assist` (mechanical, citation
  formatting, bounded copyedit) and `CitationInsert`; DOI verification waits
  for `attest`.
- The standards survey stays as a reference section; only ACL 2023, Nature's
  former copy-editing exemption, Elsevier and the Authors Guild actually shape
  `light-edit-v1`.

## Sequencing change: triage before the ledger

The proposal put all cleanup last, so that moving notes could not launder
authorship. Deleting a note and turning a repo mirror into a pointer launder
nothing, and they are most of the cleanup (the camdl, ramsey and analog-nn
spec dumps are nearly all of the 25 largest notes). So: triage first (keep /
delete / pointer / merge-later, one key per note, git keeps deletions), then
bootstrap the ledger on the survivors (~168 → ~90 notes), then merges and
moves through the ledger.

## Bugs and stale claims found along the way

- `restore()` commits as human (verified).
- Approving a Move draft deletes the source note, and `review.rs` commits only
  the target; the watcher later commits the deletion as human (reported by the
  soundness review; consistent with `drafts.rs:313`).
- The vault's `.claude/settings.local.json` pre-approves `vault_create` and
  `vault_update` for every Claude Code session (verified) — part of why agents
  wrote freely.
- The vault's own `CLAUDE.md` is stale (says ~55 notes; promises blame-based
  provenance).
- README claims that are false today: "AI output lands in drafts, never the
  vault unreviewed" (183 direct `arcana-ai` commits; the MCP instructions say
  to skip drafts; repeated in CLI help, `main.rs:64`); "every change is
  git-committed … `git blame` attributes each line" (incident); the MCP tool
  list omits three tools; "four Rust crates" (five, one a placeholder).

## iCloud is a known cause, not a guess

Three independent reports match what the server log shows:

- Kopia users hit "Resource deadlock avoided" on files in iCloud-synced folders
  whose content macOS had evicted to placeholders (kopia.discourse.group,
  "Kopia macOS resource deadlock errors", Jan 2024) — the exact error in
  `arcana-serve.log`.
- anthropics/claude-code issue #47241 (macOS 15, closed "not planned") reports
  that with Desktop & Documents sync on, `fileproviderd` silently reverts
  shell `mv`/`rm`/`git mv` operations seconds later, so git's index and the
  working tree diverge.
- A. Chandra, "A side effect of storing a git repository in iCloud Drive"
  (dev.to, Jul 2023), an Obsidian vault in iCloud: iCloud conflict copies
  appeared inside `.git/refs/heads`, giving `fatal: bad object`.

## Sources

- iA Inc., "Authorship in Times of Artificial Intelligence", 30 Nov 2023,
  https://ia.net/topics/ia-writer-7; Markdown Annotations v0.2,
  https://github.com/iainc/Markdown-Annotations
- Grammarly Authorship, https://support.grammarly.com/hc/en-us/articles/29548735595405;
  J. Bailey, https://www.plagiarismtoday.com/2025/11/06/how-grammarly-launders-ai-generated-content/
  and https://www.plagiarismtoday.com/2025/12/11/grammarly-updates-authorship-improves-labeling/
- Ink & Switch: Upwelling (2023), https://www.inkandswitch.com/upwelling/;
  Peritext (2021), https://www.inkandswitch.com/peritext/; Patchwork notebook,
  https://www.inkandswitch.com/patchwork/notebook/2024-version-control/
- git-ai, https://usegitai.com/docs/get-started/how-git-ai-works; Agent Trace
  v0.1.0, https://agent-trace.dev/; git notes, https://git-scm.com/docs/git-notes
- diff-match-patch, https://github.com/google/diff-match-patch; JGit
  HistogramDiff javadoc; tuicr, https://github.com/agavra/tuicr;
  `git add -p`, https://git-scm.com/docs/git-add
- iCloud: https://kopia.discourse.group/t/kopia-macos-resource-deadlock-errors/2574;
  https://github.com/anthropics/claude-code/issues/47241 (read via a mirror);
  https://dev.to/architchandra/a-side-effect-of-storing-a-git-repository-in-icloud-drive-7ed
