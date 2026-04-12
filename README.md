# Cairn

> Cairns are stone stacks hikers build to mark trails through unfamiliar terrain. You build them as you go. Each one is personal — every hiker stacks differently. They accumulate over time. They don't describe the landscape; they say *"I was here, go this way."*

Cairn is a **personal knowledge graph** that an AI coding agent uses to accumulate, retrieve, and refine understanding of a large codebase over time.

It is **not** shared documentation. It is one developer's evolving map of the codebase — detailed descriptions of every module and feature, opinions, war stories, gotchas, discovered dependencies — stored in a structured graph and written in that developer's voice. Think of it as **a granular, interconnected CLAUDE.md for your entire monorepo** that the agent builds and maintains as it works.

## The problem

AI coding agents start every session from zero. They re-read the same files, re-discover the same relationships, and make the same mistakes. CLAUDE.md helps, but it's a flat file — it can't capture the web of dependencies, contradictions, and hard-won lessons that make a senior engineer effective in a large codebase.

In a large monorepo, the problem is worse. There are hundreds of modules, services, and features. No one person holds the full map in their head. The agent needs to build that map as it works — and keep it across sessions.

## What Cairn does

Cairn gives your AI agent a **persistent, queryable memory** that grows organically through normal work. It serves two purposes:

### 1. Codebase atlas

As the agent works through code, it catalogues what it finds — building a detailed, interconnected map of the codebase. Each significant module, feature, service, or logical chunk gets its own topic with a description of what it does, how it works, where the key files are, and why it's built that way. Topics are linked with typed edges that capture dependencies, data flows, and ownership. Think of it as **granular, nested CLAUDE.md files for every part of the codebase, connected into a graph**.

### 2. Institutional memory

Beyond structure, the agent records the things that aren't in the code: why something was built a certain way, what broke last month, which abstractions are premature, what gotchas to watch for. Opinions, war stories, and lessons learned — in your voice.

### How it works

- **Topics** are the primary unit of knowledge — "payments/retry", "auth/oauth", "infra/event-bus"
- **Edges** connect topics with typed relationships — `depends_on`, `gotcha`, `war_story`, `contradicts`, `replaced_by`, `see_also`, `owns`
- **Blocks** are ordered chunks of content within a topic, each with an optional voice/mood annotation
- **Voice** captures your personality, preferences, and coding style so the agent writes entries in your voice
- The agent **primes itself** at the start of every task by searching the graph for relevant context
- The agent **catalogues and learns automatically** during work — describing modules it reads and recording non-obvious insights
- **Hooks** checkpoint the graph periodically and before context compaction, so nothing is lost

After a few sessions, the graph is already useful. By session ten, the agent knows the structure of your codebase, which modules are fragile, why that abstraction exists, what broke last month, and who owns what — without being told.

## Architecture

```
                    ┌─────────────────────────────────────────┐
                    │              cairn-core                 │
                    │   ops · prime · search · snapshot       │
                    │              db.rs                      │
                    │   (SurrealDB embedded, single writer)   │
                    └────────────────┬────────────────────────┘
                                     │
                              cairn-server
                            (Unix-socket daemon,
                             owns the DB exclusively)
                                     │
                  ┌──────────────────┼──────────────────┐
                  │                  │                  │
             cairn-mcp          cairn-cli          cairn-tui
            (stdio MCP)         (shell CLI)      (interactive)
```

- **cairn-core** — Rust library crate with all graph logic
- **cairn-mcp** — MCP server binary (JSON-RPC over stdio), used by Claude Code
- **cairn-cli** — CLI binary, used by you and by hook scripts
- **cairn-server** — single-writer daemon that owns the DB and serves all clients over a Unix socket (auto-spawned by clients on first use)
- **cairn-tui** — interactive terminal UI for browsing and editing the graph (vim-like editor, command palette, exclusive edit-mode lock)
- **SurrealDB embedded** — in-process database, no server, no network, single artifact on disk

## Setup

### Build from source

```bash
git clone https://github.com/youruser/cairn.git
cd cairn
./install.sh
```

`install.sh` builds the release binaries and copies `cairn-cli`, `cairn-mcp`, `cairn-server`, and `cairn-tui` to `~/.local/bin/`, then installs the hook scripts to `~/.cairn/hooks/`. If `~/.local/bin/` isn't on your `PATH`, the script tells you how to add it.

If you'd rather install manually:

```bash
cargo build --release
cp target/release/cairn-{cli,mcp,server,tui} ~/.local/bin/
```

### Initialize your graph

A Cairn graph belongs to a specific project tree. From the **root of the repo** you want to track, run:

```bash
cairn-cli init --voice "I'm a backend engineer who values explicit error handling. \
I prefer composition over inheritance. When in doubt, write a comment explaining WHY."
```

This creates `./.cairn/` in the current directory with the database inside. From then on, every Cairn binary invoked anywhere inside that tree (`cairn-cli`, `cairn-mcp`, `cairn-tui`, the hook scripts) walks up from `cwd` looking for `.cairn/` — same way `git` finds `.git/` — and connects to it automatically. No `--db` flag, no environment variable.

Cairn deliberately does **not** fall back to `~/.cairn/cairn.db`. If you want a global graph that follows you across projects, opt in explicitly:

```bash
export CAIRN_DB="$HOME/.cairn/cairn.db"
cairn-cli --db "$CAIRN_DB" init
```

Most things you care about — snapshots, hooks, logs — live next to the database under `.cairn/` in the project, not in your home directory.

### Bootstrap the initial taxonomy (optional)

An empty graph works fine — the agent builds the taxonomy as it works. But you can give it a head start:

**Option A: Auto-scan** — Install a Claude Code agent that will crawl the repo and build the taxonomy for you:

```bash
cairn-cli init --taxonomy scan
```

This copies the **taxonomer** agent to `.claude/agents/taxonomer.md`. Run it in Claude Code with `/agents/taxonomer`. It will ask you two questions before starting:

1. **How granular should the taxonomy be?** (shallow / medium / deep)
2. **Any areas to skip or focus on?** (vendored deps, generated code, specific subdirs)

Then it recursively scans the codebase, reads key files, and populates the graph with topics and connections matching the granularity you chose.

**Option B: Describe the structure** — Seed the top-level domains manually:

```bash
cairn-cli init --taxonomy "Payments, Auth, Infrastructure, Data Pipeline"
```

This creates a root topic for each domain. The agent will fill in subtopics as it works under these established categories.

**Option C: Start empty** — Just `cairn-cli init` with no `--taxonomy` flag. The agent figures out the structure on its own.

### Maintaining the taxonomy

Two additional agents ship with Cairn for keeping the graph healthy as the codebase evolves. They live in `agents/` in the Cairn repo — copy them into your project's `.claude/agents/` to use them:

```bash
cp agents/taxonomer-explode.md agents/taxonomer-verify.md /path/to/your/project/.claude/agents/
```

**`taxonomer-explode`** — Take a single existing topic that has become too broad and recursively expand it into a tree of more granular sub-topics. Useful when your initial scan was shallow and you want to drill into specific areas without re-scanning the whole repo. Run it with `/agents/taxonomer-explode`. It will ask which topic to expand, how deep to recurse, and which sub-areas to skip.

**`taxonomer-verify`** — Walk the existing graph and report issues without making changes. Detects:
- **Stale topics** whose underlying code has changed since the topic was last updated
- **Broad leaves** that should probably be exploded
- **Orphans** (topics with no edges)
- **Dead links** to file paths that no longer exist
- **Self-contradictions** between blocks in the same topic
- **Cycles** in `depends_on` chains

Run it with `/agents/taxonomer-verify`. It produces a report grouped by issue type with suggested actions — you decide what to fix.

### Connect to Claude Code

Register the MCP server so Claude Code can use the graph tools:

```bash
claude mcp add cairn -- cairn-mcp
```

`cairn-mcp` discovers the database the same way the CLI does — it walks up from Claude Code's working directory until it finds `.cairn/`. So one MCP registration works for every project that has a Cairn graph; nothing is hardcoded to a single path. Pass `--db /absolute/path` only if you want to pin a specific graph (e.g., for a global/home graph).

### Install hooks (optional but recommended)

`install.sh` already installed the hook scripts to `~/.cairn/hooks/`. To wire them into Claude Code, add the following to `~/.claude/settings.json` (user-level) — one entry covers every project:

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "$HOME/.cairn/hooks/cairn_save_hook.sh"
          }
        ]
      }
    ],
    "PreCompact": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "$HOME/.cairn/hooks/cairn_precompact_hook.sh"
          }
        ]
      }
    ]
  }
}
```

The hook scripts:
- Locate `cairn-cli` automatically (PATH lookup or fallback to `~/.local/bin/cairn-cli`)
- Walk up from `$PWD` looking for a `.cairn/` directory and use the graph they find. **If no `.cairn/` exists in any ancestor, the hook exits silently** — so a Stop hook firing in a non-Cairn repo never accidentally creates an empty graph in your home directory
- Generate session IDs with consistent formatting
- Redirect errors to `$HOME/.cairn/logs/hook.log` so they never pollute Claude Code's UI
- Use `|| true` so a failed checkpoint never blocks the agent

To pin the hooks to a specific graph regardless of cwd, set `CAIRN_DB=/absolute/path/to/cairn.db` in the hook command. To override the binary location, set `CAIRN_CLI=/path/to/cairn-cli`.

### Updating Cairn

When you pull new changes to the Cairn repo, three things may need updating:

1. **Binaries** — rebuild and reinstall:
   ```bash
   cd /path/to/cairn && ./install.sh
   ```

2. **Agent files** in your project — re-install the bundled taxonomer agents (run from your project root):
   ```bash
   cairn-cli install-agents
   ```
   This copies all bundled agents into `./.claude/agents/`, overwriting any existing versions.

3. **Database schema** — opens automatically apply forward migrations. If your binary is older than your DB, opening will fail with a clear error telling you to update.

### Health check

`cairn-cli doctor` reports the binary version, schema compatibility, and whether your installed agent files match the bundled versions:

```
$ cairn-cli doctor
Cairn doctor

Binary:
  cairn-cli version: 0.1.0
  schema support:    v1

Database (./.cairn/cairn.db):
  schema version:    v1
  status:            OK

Agents in ./.claude/agents:
  ✓ taxonomer.md           match
  ✗ taxonomer-explode.md   differs from bundled — run `cairn-cli install-agents`
  · taxonomer-verify.md    missing
```

Run it after a Cairn update to confirm everything is in sync.

## How Claude Code uses Cairn

Once the MCP server is connected, the agent operates automatically via the **behavioral contract** — a set of instructions returned by `graph_status` that tells the agent how to use the graph. No prompting required.

The contract is **context-adaptive**: it adjusts its guidance based on the graph's health. A sparse graph (< 5 topics) gets a "bootstrapping" preamble urging aggressive cataloguing. A graph with > 30% stale topics gets a staleness warning. `prime` injects per-task notes when matched topics are stale or when no topics match at all. A healthy, well-maintained graph gets a clean contract with no situational noise.

### Automatic workflow

1. **At task start**, the agent calls `graph_status` to get the contract and your voice, then calls `prime` with the task description to retrieve relevant context
2. **During work**, the agent calls `learn` when it discovers something non-obvious, `connect` when it finds relationships, and `amend` when prior knowledge is wrong
3. **At session end**, the Stop hook calls `checkpoint` to persist everything
4. **Before context compaction**, the PreCompact hook emergency-flushes to prevent knowledge loss

### What the agent records

The agent builds two layers of knowledge:

**Codebase structure** — as it reads and modifies code, it creates topics that describe what each logical area does, how it's organized, and how it connects to other areas:

- "payments/retry: Handles failed payment retries with exponential backoff + jitter. Entry point is `RetryWorker` in `payments/retry/worker.rs`. Pulls from the `payment_events` SQS queue. Max 5 retries, then DLQ. Config in `payments/retry/config.toml`."
- "auth/oauth: OAuth2 integration with Google and GitHub. Uses the `oauth2` crate. Token refresh is handled by `TokenManager` which runs as a background task. Scopes are defined per-provider in `auth/providers/`."

**Insights and opinions** — things that aren't in the code itself:

- "This retry logic is fragile because the DLQ silently swallows exceptions when full"
- "The auth middleware was rewritten for compliance, not tech debt — scope decisions should favor compliance"
- "billing-retry depends on event-bus-serialization because retry logic reads the format header"

The agent writes in **your voice**, using the personality you set during init. It uses **hierarchical topic keys** (e.g., `payments/retry`, `payments/webhooks`, `auth/oauth`) to reflect the logical structure of the codebase.

### MCP tools available to the agent

| Tool | Purpose |
|------|---------|
| `graph_status` | Returns stats, behavioral contract, and voice. Called first. |
| `prime` | Composes relevant context for a task. Called at task start. |
| `learn` | Records a new insight or extends an existing topic. |
| `connect` | Creates a typed edge between two topics. |
| `amend` | Corrects a specific block within a topic. |
| `search` | Full-text search across all topics. |
| `explore` | Shows all edges and neighbors of a topic. |
| `path` | Finds how two topics are connected through the graph. |
| `nearby` | Returns all topics within N hops, grouped by edge type. |
| `checkpoint` | Persists session state (called by hooks). |
| `snapshot` | Creates a named full backup. |
| `restore` | Restores from a snapshot (destructive). |
| `forget` | Marks a topic as deprecated (soft delete). |
| `rename` | Renames a topic key. Edges are preserved automatically. |
| `rewrite` | Replaces all blocks in a topic. |
| `set_tags` | Replace a topic's tags. |
| `set_summary` | Replace a topic's search summary. |
| `disconnect` | Remove a single edge between two topics. |
| `delete_block` | Remove a block from a topic (content saved to history). |
| `move_block` | Reorder a block within a topic without losing its ID. |
| `history` | Shows the mutation audit log. |
| `stats` | Graph overview with counts and rankings. |
| `voice` | Reads or updates your voice/personality. |

## How you use Cairn (CLI)

The CLI gives you direct access to everything the agent can do, plus some extras.

### Cataloguing the codebase

```bash
# Describe a module — the agent does this automatically, but you can too
cairn-cli learn payments/retry "Handles failed payment retries with exponential backoff + jitter. \
Entry point is RetryWorker in payments/retry/worker.rs. Pulls from the payment_events SQS queue. \
Max 5 retries, then DLQ. Config in payments/retry/config.toml." \
  --title "Payment retry mechanism" \
  --tag payments --tag retry

# Describe a related module
cairn-cli learn payments/webhooks "Receives payment provider webhooks (Stripe, PayPal). \
Validates signatures, normalizes events, and publishes to the event bus. \
Entry point: WebhookController in payments/webhooks/controller.rs." \
  --title "Payment webhooks" \
  --tag payments --tag webhooks

# Link them
cairn-cli connect payments/retry payments/webhooks see_also \
  --note "Webhook failures can trigger retries; shared payment event schema"
```

### Recording insights

```bash
# Add an insight to an existing topic
cairn-cli learn payments/retry "The DLQ consumer runs every 15 minutes. \
It silently drops messages older than 7 days — this is intentional but undocumented."

# Record a dependency
cairn-cli connect payments/retry infra/event-bus depends_on \
  --note "Retry logic reads the event bus serialization format header to determine replay strategy"

# Record a gotcha
cairn-cli connect payments/retry payments/idempotency gotcha \
  --note "Must check idempotency key before retrying — otherwise duplicate charges" \
  --severity high

# Record a war story
cairn-cli connect payments/retry incidents/march-dlq war_story \
  --note "DLQ overflow in March caused 2,000 lost payment events"
```

### Searching and exploring

```bash
# Full-text search
cairn-cli search "retry"

# Explore a topic's neighborhood
cairn-cli explore payments/retry --depth 2

# Find how two topics are connected
cairn-cli path payments/retry infra/monitoring

# See everything nearby, grouped by edge type
cairn-cli nearby payments/retry --hops 3
```

### Viewing the graph

```bash
# Full graph as a unicode tree diagram
cairn-cli view
```

Output:
```
Cairn Graph: 15 topics, 22 edges

core/
├── data-model - Data model
│   └── see_also ← core/ops, core/types, core/search
├── db - Database layer (db.rs)
│   ├── depends_on ← core/search, core/ops, core/snapshot
│   └── ⚠ gotcha → surrealdb/gotchas
├── facade - Cairn facade (lib.rs)
│   ├── depends_on ← mcp, cli
│   └── depends_on → core/prime, core/search, core/ops, core/snapshot
...
```

```bash
# Quick status
cairn-cli status

# Detailed stats
cairn-cli stats

# Mutation history
cairn-cli history
cairn-cli history payments/retry --limit 5
```

### Managing your voice

```bash
# See your current voice
cairn-cli voice

# Update it
cairn-cli voice set "I'm a backend engineer. I write Rust and Go. \
I value explicit error handling and think most abstractions in our codebase are premature."

# Edit in your $EDITOR
cairn-cli voice edit
```

### Backup and restore

```bash
# Create a named snapshot
cairn-cli snapshot --name before-refactor

# Restore from it (creates a safety snapshot first)
cairn-cli restore before-refactor

# Export the entire graph as JSON (for migration or inspection)
cairn-cli export > graph.json

# Import into a fresh database
cairn-cli --db /path/to/new.db import graph.json
```

### Correcting and updating

```bash
# Amend a specific block
cairn-cli amend payments/retry b_20260409_143200 \
  "The DLQ consumer now runs every 5 minutes (changed in v2.3)" \
  --reason "Frequency changed in the March hotfix"

# Rename a topic key (edges are preserved automatically)
cairn-cli rename payments/billing-retry payments/retry

# Deprecate a topic that's no longer relevant
cairn-cli forget auth/legacy-oauth --reason "Replaced by Clerk. See topic: auth/clerk"

# Completely rewrite a topic
echo "Everything changed in v3.0. The retry mechanism is now..." | \
  cairn-cli rewrite payments/retry --reason "Complete redesign in v3.0"
```

### JSON output

Every command supports `--json` for machine-readable output:

```bash
cairn-cli status --json
cairn-cli search "retry" --json
cairn-cli stats --json
```

## How you use Cairn (TUI)

`cairn-tui` is a full interactive terminal editor for your knowledge graph. Launch it from anywhere inside a project with a `.cairn/` directory:

```bash
cairn-tui
```

### Browsing

The TUI opens in browse mode with two panes: a **topic list** on the left and a **detail view** on the right. Navigate with:

| Key | Action |
|-----|--------|
| `j/k` | Move up/down in the focused pane |
| `Tab` | Switch focus between left and right panes |
| `Shift+Tab` | Cycle detail tabs (detail / neighbors / history) |
| `1/2/3` | Jump to detail / neighbors / history tab |
| `h/l` | Switch pane focus (h=left, l=right) |
| `/` | Filter topics by name |
| `?` | Full-text search (FTS) |
| `Enter` | Open context menu for the selected element |
| `:` | Open the command palette |
| `R` | Refresh all data from the daemon |

The right pane's detail view has selectable elements — navigate to a specific block, edge, title, or tag line and press `Enter` to see actions relevant to that element.

### Edit mode

Press `e` (or `Enter` → "Enter edit mode" from the context menu) to acquire an **exclusive editor lock** on the daemon. While you're editing, AI agents are blocked from writing — they can still read (`prime`, `search`, `stats`), but mutations return `EditorBusy` until you release the lock.

A red confirmation dialog appears before the lock is acquired. The header shows `[EDIT MODE]` while active. Press `Esc` to release the lock and return to browse mode.

### Editing operations

All operations are accessible via direct keybinds in edit mode AND through the command palette (`:` → type to filter → Enter):

| Key | Operation |
|-----|-----------|
| `e` | **Amend block** — edit a block's content in the vim-like editor |
| `b` | **Add block** — append a new block to the selected topic |
| `D` | **Delete block** — remove a block (with mandatory reason) |
| `K/J` | **Move block** up/down within a topic |
| `r` | **Rename topic** |
| `d` | **Forget topic** — soft-delete with reason |
| `t` | **Edit tags** — comma-separated |
| `s` | **Edit summary** — in the full editor |
| `V` | **Edit voice** — the developer personality |
| `n` | **Learn new topic** — key → title → content |
| `a` | **Add edge** — fuzzy topic picker → edge type → note |
| `x` | **Remove edge** — pick from the topic's edges |

When the right pane has a specific element selected (e.g., a block), pressing `Enter` shows a **context-sensitive menu** with only the relevant actions — and if you're not in edit mode, it prompts you to enter it first, then flows directly into the action.

### The text editor

Block content, voice, and summaries are edited in a **vim-like modal editor** built on `tui-textarea`:

| Mode | Indicator | Behavior |
|------|-----------|----------|
| **NOR** (Normal) | Cyan `[NOR]` | `:` for commands, `i` for insert, `hjkl` movement, `g/G` top/bottom |
| **INS** (Insert) | Green `[INS]` | Type freely, `Esc` returns to Normal |
| **CMD** (Command) | Yellow `[CMD]` | `:w` save, `:q` quit, `:wq` save+quit, `:q!` force quit |

The editor starts in Normal mode. `:w` saves and keeps the editor open (for voice/summary/learn) or updates the baseline (for amend — the actual save happens on `:wq` when the reason prompt appears). `:q` refuses to close if there are unsaved changes; use `:q!` to discard.

### Command palette

Press `:` in browse mode to open the command palette — a fuzzy-filtered list of every available action. **Context-relevant actions sort to the top** based on the currently selected element, so the most useful commands are always first. Type to narrow, `j/k` to navigate, `Enter` to dispatch.

## Edge types

| Edge | Meaning | Example |
|------|---------|---------|
| `depends_on` | A requires B to function | payments/retry → infra/event-bus |
| `gotcha` | B is a known pitfall when working with A | payments/retry → payments/idempotency |
| `war_story` | B is an incident related to A | payments/retry → incidents/march-dlq |
| `contradicts` | A and B contain conflicting information | specs/old-api → specs/new-api |
| `replaced_by` | A is outdated; B is current | auth/legacy-oauth → auth/clerk |
| `see_also` | Loose association | infra/event-bus → infra/monitoring |
| `owns` | Ownership/responsibility | payments/retry → teams/payments |

## Data location

A Cairn graph lives **inside the project tree**, next to the code it describes:

```
<repo-root>/
└── .cairn/
    ├── cairn.db/          # SurrealDB data (the single artifact)
    ├── cairn.sock         # Unix socket — daemon ↔ clients
    ├── .cairn.db.lock     # Single-writer flock
    └── snapshots/         # Named backups
        └── manifest.json  # Snapshot index
```

Hook logs and the daemon log still go under `~/.cairn/logs/` because they're not tied to a specific graph.

Every Cairn binary discovers the database by walking up from the current working directory looking for a `.cairn/` directory (the same way `git` finds `.git/`). If none is found, the binary refuses to silently create one in your home directory — you must `cairn-cli init` from a project root, or set `CAIRN_DB`/pass `--db` explicitly to opt into a global graph.

Override paths anywhere with `--db /absolute/path` or the `CAIRN_DB` environment variable.

## Design principles

1. **Single artifact.** The entire knowledge base is one SurrealDB directory. Copy it, back it up, move it to another machine.
2. **Zero-prompt operation.** The agent knows what to do without being told. The behavioral contract is embedded in the `graph_status` response.
3. **Semantic tools, not CRUD.** The agent thinks in terms of `learn`, `connect`, `amend` — never "create node" or "insert edge."
4. **Your voice.** Entries carry tone, opinion, and personality. *"This module is a nightmare, the abstraction is wrong, but here's how to survive it"* is a valid and encouraged entry.
5. **Graceful cold start.** An empty graph returns nothing from `prime`. The agent works normally. By session three the graph is already useful.
6. **No cloud.** Everything is local. There is a small Unix-socket daemon (`cairn-server`) because SurrealKV is single-writer and multiple Claude Code sessions need to share one graph, but it's auto-spawned on first use by any client and holds an exclusive flock. You never start it manually; `install.sh` SIGTERMs the running daemon on upgrade and clients auto-reconnect transparently.
7. **Human in the loop.** The TUI lets you browse, edit, and curate the graph directly — without the agent. An exclusive editor-session lock blocks agent writes while you're editing, but reads stay available. The graph is your personal database; the agent is one writer among two.

## Platform support

- macOS (Apple Silicon and Intel)
- Linux (x86_64 and aarch64)

Windows is not supported.

## License

[MIT](LICENSE) © 2026 Michal Frdlik
