# AI Pipelines

Arcana's AI features follow a plan-first, human-in-the-loop workflow. Planning is cheap; generation is expensive. Every pipeline works the same way:

1. **Gather** — read inputs (project files or vault notes), build context
2. **Plan** — AI proposes what to create, show the plan and estimated cost
3. **Decide** — review: generate, edit the plan in `$EDITOR`, or quit
4. **Generate** — AI writes drafts to `.arcana/drafts/<session>/`
5. **Review** — `arcana review` to approve/reject/edit each draft

## Ingest

Reads an external codebase and authors new vault notes from scratch. The AI explores the project, identifies key concepts, and produces self-contained reference notes.

```bash
arcana ingest ~/code/my-project
arcana ingest ~/code/my-project --skill model-extract
arcana ingest ~/code/my-project --auto          # skip interactive prompt
arcana ingest ~/code/my-project --profile opus   # use a specific LLM profile
```

### Phases

1. **Explore** — agentic multi-turn loop. The LLM reads project files via tools (`project_tree`, `project_read`, `vault_search`), checks the vault for existing coverage, and produces a summary.

2. **Plan** — takes the summary + vault context and outputs a structured JSON plan: notes to create, their paths, titles, and source files. You can edit the plan in `$EDITOR` before proceeding.

3. **Generate** — iterates over the plan. For each note, re-reads source files and generates formatted markdown with frontmatter, wikilinks, and AI provenance metadata.

### Skills

Skills are markdown files in `.arcana/skills/` that teach the AI how to extract knowledge for a specific domain. Load with `--skill <name>`:

```bash
arcana skills                        # list available skills
arcana ingest . --skill model-extract
```

A skill might instruct the AI to look for model equations, parameters, and assumptions in scientific code — domain knowledge that a generic prompt wouldn't capture.

### Show prompts

To see (and optionally override) the prompts used at each phase:

```bash
arcana ingest --show-prompt
```

This prints the default prompt for each phase along with the file path where you can save an override (e.g. `.arcana/prompts/explore.md`).

## Review

All AI output lands in drafts — never directly in your vault.

```bash
arcana review                         # review all pending drafts
arcana review --verify-style          # also run style guide checks
```

For each pending draft, review shows a diff and prompts:

- **[a]pprove** — move to vault, reindex, git commit with AI author
- **[d]elete** — reject and remove the draft
- **[s]kip** — leave pending for later
- **[A]pprove all** — approve remaining drafts

### Style verification

`--verify-style` runs two checks before showing each draft:

1. **Heuristic checks** — missing frontmatter, missing title/tags, filename conventions, stub detection
2. **LLM verification** — sends the draft + your style guide to the LLM for a pass/fail assessment

## Chat

Interactive multi-turn chat with your vault. The AI can search, read, and draft notes during conversation.

```bash
arcana chat
arcana chat --profile opus
```

Commands within chat:
- `/clear` — reset conversation
- `/quit` — exit

## Prompt overrides

Every pipeline phase has a default task prompt. You can override any of them by placing a markdown file in `.arcana/prompts/`:

| File | Pipeline | Phase |
|------|----------|-------|
| `explore.md` | ingest | explore |
| `ingest-plan.md` | ingest | plan |
| `ingest-generate.md` | ingest | generate |
| `chat.md` | chat | system prompt |

Use `--show-prompt` to see the defaults:

```bash
arcana ingest --show-prompt
```

## Provenance

Git tracks who wrote what. AI-generated content uses a distinct author identity:

```
Human edits:  Your Name <you@example.com>
AI drafts:    arcana-ai <ai@arcana.local>
```

After approval, `git blame` shows line-level attribution:

```bash
arcana blame notes/rust/ownership.md   # human vs AI per line
arcana log notes/rust/ownership.md     # commit history
arcana diff notes/rust/ownership.md    # uncommitted changes
arcana restore notes/rust/ownership.md abc123  # restore previous version
```
