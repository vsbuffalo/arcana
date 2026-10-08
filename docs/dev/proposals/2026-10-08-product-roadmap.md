# From personal tool to product: what makes arcana usable by others

Date: 2026-10-08
Status: proposal — for discussion

## Problem

Arcana now does what it is for: agents keep a textbook and records for one
person, every word has a recorded author, and the person's own words are
changed only with their consent. Getting it running, however, took its author
an afternoon of launchd edits, a vault move out of iCloud, a hostname setting
nobody had documented, and an OAuth bug that blocked every client registering
itself. Each of those is a place another user would stop. This proposal ranks
the work by how much of that friction it removes.

## 1. Install and first run

Done: `arcana setup` (vault, server settings with secrets out of the service
definition, launchd or systemd, Claude Code registration, health check;
idempotent, `--dry-run`) and `install.sh` (cargo build, then setup).

Next:

- Prebuilt binaries for macOS (arm64, x86_64) and Linux on each tagged
  release, so installing does not need a Rust toolchain; a Homebrew tap on top.
  `cargo-dist` produces both from a single config.
- `arcana doctor`: one report of everything setup assumes and that can
  later break. That covers: the vault is not in a synced folder; it has a git
  remote; the service is running the installed binary; the public hostname
  resolves and passes the Host check; agents cannot write the vault directly.

## 2. Remote access

Issue #5. Users should not have to build a tunnel by hand. In order of
effort: `--funnel` (Tailscale Funnel), `--cloudflare <host>`, then an optional
hosted relay. Also: make refused hostnames loud, persist access tokens so a
restart does not sign every client out, and check the public path in `doctor`.

## 3. Review where people already are

The terminal review is right for its author and wrong for almost everyone
else, and it cannot be reached from a phone.

- **Obsidian plugin.** It colours text by author, using the same
  palette as `arcana blame`, and shows suggestions to the user's own words
  inline with accept and reject. It writes decisions through the local
  server, so the ledger stays the single write path. Most users of a
  markdown vault already live in Obsidian, so this is the largest single gain.
- **Web review page** served by the arcana server and reached over the same
  remote access, with decisions bound to a credential that agents do not
  hold. This is the phone path.

## 4. Integrations

Thin packages in `integrations/`, versioned with the server so they never
disagree about the tool set.

- **Claude Code plugin:** the MCP connection plus a few skills that teach the
  workflow, e.g. "log this measurement to my lab notebook", "explain this using
  my lab notes", "what is waiting for my review". It can also carry a
  SessionStart hook that reports pending suggestions. One command installs all
  of it, where today it takes `claude mcp add` and reading the docs.
- **ChatGPT app:** no code beyond the server; a name, icon and description
  for listing. Connecting does not need it: the failure the author hit was
  arcana's OAuth refusing self-registered clients (fixed in `8fec2ef`). An app
  listing helps others find arcana. Publishing requires OpenAI's review.

## 5. The textbook itself

From the attribution-ledger proposal, not yet built:

- Questions: `?` in review, `CC?` marks in a note, or `ask` from an agent,
  anchored to a block and answered by an edit that names the question.
- `reader.md` and per-subject outlines marking concepts known / shaky / new,
  returned to agents so explanations are pitched at the reader.
- `arcana attest <post>` for publishing: per-sentence authorship under a named
  policy, plus a short disclosure paragraph.

## 6. Documentation

A README written around the guarantee ("agents write for you, never over
you"), the quick setup, and connecting each client; the stale claims about
drafts and git-blame provenance removed.

## Order of work

1. Owner's machine on `arcana setup` (dry run reviewed; run it).
2. Persist access tokens; loud Host refusals (#5 checklist).
3. Prebuilt binaries + Homebrew; README rewrite.
4. Claude Code plugin in `integrations/`.
5. Web review page, then the Obsidian plugin.
6. Questions, `reader.md`, `attest`.
7. Tunnel helpers, then decide on a hosted relay.

## Decisions

1. Whether a hosted relay is in scope at all, since it is a service to run.
   Need the owner.
2. Obsidian plugin before or after the web review page. Recommend web review
   first (leaning): it reuses the server, and it is the phone path the owner
   needs now.
