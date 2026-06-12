# Configuration

Arcana uses TOML config files with a layered merge system.

## Config hierarchy

```
compiled defaults
  └─▸ ~/.config/arcana/config.toml      global
       └─▸ <vault>/.arcana/config.toml  vault-local
            └─▸ CLI flags / env vars    highest priority
```

Each layer deep-merges into the previous one. A vault-local config with just `[agent.ingest]` won't clobber your global `[profiles]`.

## Config file location

The global config lives at `$XDG_CONFIG_HOME/arcana/config.toml` (defaults to `~/.config/arcana/config.toml`).

Vault-local config lives at `<vault>/.arcana/config.toml`.

Override with `--config /path/to/config.toml` (skips merge, uses only that file).

## Full reference

```toml
# ─── Vault ────────────────────────────────────────────────────────────
[vault]
path = "/path/to/your/vault"

# Directories to exclude from indexing (in addition to dotfiles)
exclude = [".obsidian", ".trash", ".arcana"]

# ─── Index ────────────────────────────────────────────────────────────
[index]
# Where to store the SQLite index
# "colocated" = <vault>/.arcana/index.db (default)
# "xdg"       = ~/.local/share/arcana/<vault-hash>/index.db
db_location = "colocated"

# Explicit path (overrides db_location)
# db_path = "/tmp/arcana-index.db"

# ─── Search ───────────────────────────────────────────────────────────
[search]
default_limit = 20        # max results returned
snippet_length = 150      # chars per search snippet

# ─── LLM profiles ────────────────────────────────────────────────────
# Named LLM configurations. Use --profile or ARCANA_PROFILE to select.
default_profile = "sonnet"

[profiles.sonnet]
provider = "anthropic"                    # anthropic | openai | ollama
model = "claude-sonnet-4-5-20250929"
# api_key_env = "ANTHROPIC_API_KEY"      # auto-resolved from provider
# endpoint = "https://custom-endpoint"   # for proxies or ollama
# max_output_tokens = 8192

[profiles.opus]
provider = "anthropic"
model = "claude-opus-4-6-20250918"

[profiles.local]
provider = "ollama"
model = "llama3.1"
endpoint = "http://localhost:11434/v1"

[profiles.openai]
provider = "openai"
model = "gpt-4o"

# Legacy: [llm] block still works if no profiles are defined
# [llm]
# provider = "anthropic"
# model = "claude-sonnet-4-5-20250929"

# ─── Agent ────────────────────────────────────────────────────────────
[agent]
max_iterations = 20       # max tool-use rounds per phase
max_tokens = 1000000      # context window budget
max_output_tokens = 8192  # max tokens per LLM response
default_tags = ["ai-generated"]

# Per-operation overrides (inherit from [agent] when unset)
[agent.ingest]
# max_iterations = 30
# max_tokens = 500000
# max_explore_iterations = 15
profile = "opus"          # use a different LLM profile for ingest

[agent.tidy]
# profile = "sonnet"

# ─── Git ──────────────────────────────────────────────────────────────
[git]
enabled = true
auto_commit = true

# How often the file watcher commits human edits (seconds).
# Default: 300 (5 min). Set to 0 for immediate commits.
commit_interval_secs = 300

# Human author identity (falls back to git config)
# user_name = "Your Name"
# user_email = "you@example.com"

# AI author identity
ai_name = "arcana-ai"
ai_email = "ai@arcana.local"

# ─── Drafts ───────────────────────────────────────────────────────────
[drafts]
retention_days = 30       # auto-prune resolved sessions after this many days
```

## Profile resolution

When arcana needs an LLM, it resolves the profile in this order (later wins):

1. `default_profile` from config
2. Per-operation default: `[agent.ingest].profile` or `[agent.tidy].profile`
3. `--profile` CLI flag or `ARCANA_PROFILE` env var
4. `--provider` / `--model` CLI flags (override individual fields)

```bash
# Uses default_profile from config
arcana ingest ~/project

# Uses the "opus" profile
arcana ingest ~/project --profile opus

# Uses "local" profile but overrides the model
arcana ingest ~/project --profile local --model phi3
```

## Brain profile

Your vault's "brain profile" lives in the vault itself:

```
<vault>/.arcana/
├── taxonomy.md    # zone definitions, routing rules for notes
├── style.md       # writing style, formatting conventions
├── skills/        # domain-specific extraction instructions
│   └── model-extract.md
└── prompts/       # override default AI task prompts
    ├── explore.md
    ├── ingest-plan.md
    ├── ingest-generate.md
    ├── tidy-audit.md
    ├── tidy-plan.md
    ├── tidy-generate.md
    └── chat.md
```

**Taxonomy** defines zones (top-level directories) and what goes where. The AI uses this to route notes and validate paths.

**Style guide** defines voice, formatting, templates. The AI follows this when writing notes.

**Skills** are domain-specific instructions loaded with `--skill <name>` during ingest.

**Prompts** override the default task prompts for each pipeline phase. Use `arcana ingest --show-prompt` or `arcana tidy --show-prompt` to see the defaults and their save paths.

## Environment variables

| Variable | Purpose |
|----------|---------|
| `ANTHROPIC_API_KEY` | API key for Anthropic provider |
| `OPENAI_API_KEY` | API key for OpenAI provider |
| `ARCANA_PROFILE` | Default LLM profile (same as `--profile`) |
| `ARCANA_BEARER_TOKEN` | Static bearer token for MCP server auth |
| `ARCANA_OAUTH_CLIENT_ID` | OAuth client ID |
| `ARCANA_OAUTH_CLIENT_SECRET` | OAuth client secret |
| `ARCANA_OAUTH_PASSWORD` | OAuth authorization password |
| `ARCANA_USER_NAME` | Git author name (same as `--name`) |
| `ARCANA_USER_EMAIL` | Git author email (same as `--email`) |
| `RUST_LOG` | Log level filter (e.g. `info`, `debug`, `arcana_server=debug`) |
