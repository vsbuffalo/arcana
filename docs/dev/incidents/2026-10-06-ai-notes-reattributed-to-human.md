# AI-written notes re-attributed to the human by a second arcana process

Date: 2026-10-06
Status: fixed in `git.rs` (uncommitted, server not yet redeployed); history repair pending
Severity: high (silently corrupts the provenance guarantee the tool exists to provide)

## What happened

`notes/fabrication/fusion-360-workflow.md` was created by the AI on 2026-05-06
(commit `7d85602`, author `arcana-ai`). Today `git blame` attributes all 326 of
its lines to Vince. Its history in the vault repo:

```
7d85602 2026-05-06 arcana-ai     create notes/fabrication/fusion-360-workflow.md   (+326)
3320905 2026-05-09 arcana-ai     create projects/baseball-am-radio/...near-field... (re-adds fusion +326, plus 3 unrelated files)
e26b029 2026-05-14 Vince Buffalo update blog-posts/ai-and-tool-building/index.md  (deletes fusion -326 and 5 other AI notes)
78c9c7c 2026-06-13 Vince Buffalo adopt 46 untracked notes                         (re-adds fusion +326 as human)
```

Across the vault, 49 tracked notes whose first commit was by `arcana-ai` are
now blamed more than half on the human — 11,672 lines in total (measured: for
each tracked `.md`, author of the `--diff-filter=A` commit vs. `git blame`
author counts). They include most of the large `projects/camdl/*` specs,
`notes/microcontrollers/*`, and both `typhoid-camdl` decision notes: the bulk of
the AI-written content in the vault is recorded as human-written.

## Root cause

`VaultGit::stage_and_commit` (`crates/arcana-core/src/git.rs:344`) builds every
commit from `self.repo.index()`, libgit2's in-memory copy of `.git/index`,
loaded once per `Repository` handle and never re-read. More than one arcana
process writes the vault: the launchd server (port 8787), the stdio MCP server
that Claude Code spawns, and `arcana review` in the CLI. Each holds its own
stale index. When process B commits, it writes *its* index back as the tree, so
any file process A committed since B loaded its index is absent from the tree:
the commit deletes it. The file is still on disk, so the next
`adopt_untracked()` tick (`git.rs:315`) commits it as human, because adoption
assumes any untracked file came from the human. The inverse also occurs: a
stale index carrying another process's staged entries bundles unrelated files
into an AI commit (`3320905`).

Reproduced by `crates/arcana-core/tests/repro_two_process_index.rs`: two
`VaultGit` handles on one repo; an AI commit through A, then a human commit
through B. Result: `ai.md` is missing from HEAD, and after adoption its
provenance reads `human_lines=1 ai_lines=0`.

## Contributing design issues

1. **Unknown writers default to human.** `adopt_untracked` and the watcher
   attribute every change arcana did not itself make to the human. Anything
   that writes the vault directly — a Claude Code `Write` call, a sync of a repo
   doc, another arcana process — is recorded as human. A guarantee of the form
   "these words are mine" cannot rest on a default of "human unless proven
   otherwise".
2. **The AI-write dedup set is per-process** (`pending_ai_writes`), so it cannot
   suppress cross-process misattribution even in principle.
3. **Line-level, inferred attribution.** `git blame` attributes whole lines by
   last commit author; a reflow or a one-word AI fix flips a human line to AI,
   and there is no way to express "human sentence, AI-inserted citation".
4. **Frontmatter provenance is not applied on the MCP path.** Only 12 of 168
   notes carry `ai:` frontmatter despite 183 `arcana-ai` commits.

The same mechanism also *reverts* content. `projects/typhoid-camdl/wenger-2026.md`
was updated by the AI on 2026-05-04 (`01b03d3`, blob `740bac2e`); the human
commit `80667e9` on 2026-05-09, which touched only blog posts, rolled HEAD back
to the creation version (`9f2e3c60`). The disk still holds the update, so git
reports the file as modified. Several of the 16 uncommitted changes in the vault
have this signature: the working tree is correct and HEAD is older.

## Second cause: the server's git calls have mostly failed since July

`~/Library/Logs/arcana-serve.log` records 2,046 `git adopt failed`, 26
`git commit failed` and 2 `git init failed` warnings, all with
`failed to read descriptor: Resource deadlock avoided` (`EDEADLK`), the first on
2026-07-09. The vault lives in `~/Documents`, which is synced by iCloud, and
`find -flags +dataless` finds files that iCloud has evicted to placeholders
(today: `.git/FETCH_HEAD`). Reading a dataless placeholder from a background
process is the usual source of `EDEADLK` on macOS — that link is inferred, not
proven. Either way the warnings never surfaced, so commits silently stopped.
Git repositories inside iCloud-synced folders are also at risk of corruption from
iCloud syncing `.git` internals file by file.

## Other observations in the vault (2026-10-06)

- 16 uncommitted working-tree changes, including +2,722 unattributed lines in
  `projects/camdl/camdl-language-spec.md`. When these are eventually committed
  by the watcher they will be attributed to the human.
- Human commit messages embed absolute paths
  (`vault: update /Users/vsb/Documents/Obsidian/...`) because
  `commit_human_change` formats the watcher's absolute paths
  (`git.rs:275`) instead of the vault-relative ones.
- An empty note at `~/projects/dotfiles.md` (a literal `~` directory in the vault
  root), which zone validation should have rejected.

## Remediation

- Done: `stage_and_commit` builds each tree from HEAD's tree plus only the named
  paths, in a throwaway in-memory index, and retries if another process moved
  HEAD in the meantime (libgit2 returns `GIT_EMODIFIED` when the tip is no
  longer the first parent). `.git/index` is re-read from disk before it is
  updated, so a stale copy is never written back. Commit messages use
  vault-relative paths. Regression tests: `crates/arcana-core/tests/git_multi_process.rs`.
- Move the vault out of iCloud-synced `~/Documents` (or mark `.git` with a
  `.nosync` exclusion), and make git failures visible rather than log-only.
- Stop defaulting unknown writers to human; see
  `docs/dev/proposals/2026-10-06-authorship-model.md`.
- Repair: the AI-origin of the affected files is still recoverable from history
  (each was introduced by an `arcana-ai` commit before the spurious deletion).
  A one-off replay can recover correct attribution.
