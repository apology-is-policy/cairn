# Cairn — Personal Agent Knowledge Graph

> Cairns are stone stacks hikers build to mark trails through unfamiliar
> terrain. You build them as you go. Each one is personal — every hiker
> stacks differently. They accumulate over time. They don't describe the
> landscape; they say "I was here, go this way."

## Philosophy

Cairn is a personal, out-of-repo knowledge graph that an AI coding agent
uses to accumulate, retrieve, and refine understanding of a large codebase
over time. It is **not** shared documentation. It is one developer's evolving
mental model — opinions, war stories, gotchas, discovered dependencies —
stored in a structured graph and written in that developer's voice.

Topics are cairns — trail markers through a massive codebase. Edges are
trails between cairns. Gotchas are warning cairns. War stories are cairns
at the site of a past rockslide.

The system is designed so that once a workspace has an active graph, the agent
uses it automatically. The developer never prompts "check the knowledge base."
The agent primes itself at task start, records insights during work, and
checkpoints before context loss. The graph grows organically through normal
work. There is no upfront population step.

### Design principles

1. **Single artifact.** The entire knowledge base is one SurrealDB file.
   Copy it, back it up, move it to another machine. Done.
2. **Zero-prompt operation.** The MCP server's `graph_status` response
   includes a behavioral contract. The agent knows what to do without being
   told.
3. **Semantic tools, not CRUD.** The agent thinks in terms of `learn`,
   `connect`, `amend` — never "create node" or "insert edge."
4. **Your voice.** Entries carry tone, opinion, and personality. This is not
   dry documentation. "This module is a nightmare, the abstraction is wrong,
   but here's how to survive it" is a valid and encouraged entry.
5. **Graceful cold start.** An empty graph returns nothing from `prime`. The
   agent works normally. By session three the graph is already useful.
6. **No cloud, no daemon.** Everything is local. The MCP server runs as a
   stdio process spawned by the AI coding agent. No ports, no auth, no
   network.

### Target platforms

- macOS (Apple Silicon and Intel)
- Linux (x86_64 and aarch64)

Windows is explicitly out of scope.

---

## Architecture

### Crate structure

```
cairn/
├── Cargo.toml              # workspace root
├── cairn-core/            # library crate — all graph logic
│   ├── src/
│   │   ├── lib.rs
│   │   ├── db.rs           # SurrealDB connection, migrations, schema
│   │   ├── ops.rs          # semantic operations (learn, connect, amend, ...)
│   │   ├── prime.rs        # context composition for agent priming
│   │   ├── search.rs       # full-text and graph traversal search
│   │   ├── snapshot.rs     # backup, restore, history
│   │   ├── protocol.rs     # behavioral contract generation
│   │   ├── types.rs        # shared types (Node, Edge, EdgeKind, ...)
│   │   └── error.rs        # error types
│   └── Cargo.toml
├── cairn-mcp/             # MCP server binary — thin wrapper
│   ├── src/
│   │   └── main.rs         # JSON-RPC stdio transport, tool dispatch
│   └── Cargo.toml
├── cairn-cli/             # CLI binary — thin wrapper
│   ├── src/
│   │   └── main.rs         # clap-based CLI, same ops as MCP
│   └── Cargo.toml
├── cairn-tui/             # TUI binary — phase 2, placeholder
│   ├── src/
│   │   └── main.rs         # ratatui-based graph navigator/editor
│   └── Cargo.toml
└── hooks/                  # shell scripts for Claude Code integration
    ├── cairn_save_hook.sh
    └── cairn_precompact_hook.sh
```

### Binary topology

One workspace, four crates, three binaries (TUI deferred):

```
┌─────────────────────────────────────────────────────┐
│                   cairn-core                       │
│                                                     │
│  ops.rs ─── prime.rs ─── search.rs ─── snapshot.rs  │
│     │                                               │
│     └──── db.rs (SurrealDB embedded)                │
│              │                                      │
│              ▼                                      │
│         cairn.db (single file)                     │
└──────────┬──────────────┬───────────────┬───────────┘
           │              │               │
    cairn-mcp      cairn-cli      cairn-tui
    (stdio MCP)     (shell CLI)     (phase 2)
```

All three binaries are thin dispatchers into `cairn-core`. Logic is never
duplicated across binaries.

### SurrealDB integration

SurrealDB runs in **embedded mode** — in-process, no server, no network. The
Rust crate `surrealdb` with the `kv-rocksdb` or `kv-surrealkv` feature
provides this. The database is a single directory on disk, but for backup
purposes we treat it as an opaque artifact (export/import via SurrealQL).

Connection pseudocode:

```rust
use surrealdb::engine::local::SurrealKV;
use surrealdb::Surreal;

let db = Surreal::new::<SurrealKV>("/path/to/cairn.db").await?;
db.use_ns("cairn").use_db("main").await?;
```

---

## Data Model (SurrealDB Schema)

### Node types

SurrealDB uses tables as record types. Each table is a node type.

```surql
-- ============================================================
-- KNOWLEDGE NODES
-- ============================================================

-- A topic node: the primary unit of knowledge.
-- Examples: "billing-retry", "event-bus-serialization", "auth-migration"
DEFINE TABLE topic SCHEMAFULL;
DEFINE FIELD key         ON topic TYPE string  ASSERT $value != NONE;
DEFINE FIELD title       ON topic TYPE string  ASSERT $value != NONE;
DEFINE FIELD summary     ON topic TYPE string  DEFAULT "";
DEFINE FIELD blocks      ON topic TYPE array<object> DEFAULT [];
  -- Each block: { id: string, content: string, voice: option<string>,
  --               created_at: datetime, updated_at: datetime }
  -- Blocks are ordered. "Insert in the middle" means splicing this array.
DEFINE FIELD tags        ON topic TYPE array<string> DEFAULT [];
DEFINE FIELD created_at  ON topic TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at  ON topic TYPE datetime DEFAULT time::now();
DEFINE FIELD deprecated  ON topic TYPE bool DEFAULT false;

DEFINE INDEX idx_topic_key ON topic FIELDS key UNIQUE;
DEFINE INDEX idx_topic_tags ON topic FIELDS tags;

-- Full-text search index on title, summary, and block content.
-- SurrealDB supports SEARCH analyzers natively.
DEFINE ANALYZER cairn_analyzer TOKENIZERS blank, class
  FILTERS lowercase, snowball(english);
DEFINE INDEX idx_topic_search ON topic FIELDS title, summary
  SEARCH ANALYZER cairn_analyzer BM25;


-- ============================================================
-- PERSONALITY / PREFERENCES
-- ============================================================

-- The voice node: always loaded during prime. Contains the developer's
-- coding style, opinions, workflow preferences, pet peeves.
-- There is exactly one of these per graph.
DEFINE TABLE voice SCHEMAFULL;
DEFINE FIELD content     ON voice TYPE string  ASSERT $value != NONE;
DEFINE FIELD updated_at  ON voice TYPE datetime DEFAULT time::now();


-- The preferences node: controls how the agent uses the graph.
-- Tuning knobs like verbosity, learn aggressiveness, prime depth.
-- There is exactly one of these per graph.
DEFINE TABLE preferences SCHEMAFULL;
DEFINE FIELD prime_max_tokens     ON preferences TYPE int    DEFAULT 4000;
DEFINE FIELD prime_include_gotchas ON preferences TYPE bool  DEFAULT true;
DEFINE FIELD learn_verbosity      ON preferences TYPE string DEFAULT "normal";
  -- "terse" | "normal" | "verbose"
DEFINE FIELD learn_auto           ON preferences TYPE bool   DEFAULT true;
  -- If false, agent only learns when explicitly asked.
DEFINE FIELD updated_at           ON preferences TYPE datetime DEFAULT time::now();


-- ============================================================
-- HISTORY / AUDIT
-- ============================================================

-- Every mutation is recorded as an event for time-travel.
DEFINE TABLE history SCHEMAFULL;
DEFINE FIELD op          ON history TYPE string;
  -- "learn" | "amend" | "connect" | "disconnect" | "forget" | "rewrite"
DEFINE FIELD target      ON history TYPE string;
  -- Record ID of the affected node, e.g. "topic:billing_retry"
DEFINE FIELD detail      ON history TYPE string  DEFAULT "";
  -- Human-readable description of what changed.
DEFINE FIELD diff        ON history TYPE option<string>;
  -- Optional: the old content for amend/rewrite ops, enabling undo.
DEFINE FIELD session_id  ON history TYPE string  DEFAULT "";
  -- Groups mutations by work session for checkpoint/rollback.
DEFINE FIELD created_at  ON history TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_history_target ON history FIELDS target;
DEFINE INDEX idx_history_session ON history FIELDS session_id;
```

### Edge types (relations)

SurrealDB models edges as typed RELATE statements between records.

```surql
-- ============================================================
-- EDGES (RELATIONS)
-- ============================================================

-- depends_on: A requires B to function or be understood.
-- Example: billing-retry depends_on event-bus-serialization
DEFINE TABLE depends_on SCHEMAFULL;
DEFINE FIELD in    ON depends_on TYPE record<topic>;
DEFINE FIELD out   ON depends_on TYPE record<topic>;
DEFINE FIELD note  ON depends_on TYPE string DEFAULT "";
DEFINE FIELD created_at ON depends_on TYPE datetime DEFAULT time::now();

-- contradicts: A and B contain conflicting information.
-- The note should explain the contradiction.
DEFINE TABLE contradicts SCHEMAFULL;
DEFINE FIELD in    ON contradicts TYPE record<topic>;
DEFINE FIELD out   ON contradicts TYPE record<topic>;
DEFINE FIELD note  ON contradicts TYPE string DEFAULT "";
DEFINE FIELD created_at ON contradicts TYPE datetime DEFAULT time::now();

-- replaced_by: A is outdated; B is the current understanding.
DEFINE TABLE replaced_by SCHEMAFULL;
DEFINE FIELD in    ON replaced_by TYPE record<topic>;
DEFINE FIELD out   ON replaced_by TYPE record<topic>;
DEFINE FIELD note  ON replaced_by TYPE string DEFAULT "";
DEFINE FIELD created_at ON replaced_by TYPE datetime DEFAULT time::now();

-- gotcha: B is a known pitfall when working with A.
-- Gotchas are directional: the "in" node is the area, "out" is the trap.
DEFINE TABLE gotcha SCHEMAFULL;
DEFINE FIELD in    ON gotcha TYPE record<topic>;
DEFINE FIELD out   ON gotcha TYPE record<topic>;
DEFINE FIELD note  ON gotcha TYPE string DEFAULT "";
DEFINE FIELD severity ON gotcha TYPE string DEFAULT "medium";
  -- "low" | "medium" | "high" | "critical"
DEFINE FIELD created_at ON gotcha TYPE datetime DEFAULT time::now();

-- see_also: loose association. A and B are related but not dependent.
DEFINE TABLE see_also SCHEMAFULL;
DEFINE FIELD in    ON see_also TYPE record<topic>;
DEFINE FIELD out   ON see_also TYPE record<topic>;
DEFINE FIELD note  ON see_also TYPE string DEFAULT "";
DEFINE FIELD created_at ON see_also TYPE datetime DEFAULT time::now();

-- war_story: B is an incident or experience report related to A.
DEFINE TABLE war_story SCHEMAFULL;
DEFINE FIELD in    ON war_story TYPE record<topic>;
DEFINE FIELD out   ON war_story TYPE record<topic>;
DEFINE FIELD note  ON war_story TYPE string DEFAULT "";
DEFINE FIELD created_at ON war_story TYPE datetime DEFAULT time::now();

-- owns: ownership/responsibility. "who owns this area"
DEFINE TABLE owns SCHEMAFULL;
DEFINE FIELD in    ON owns TYPE record<topic>;
DEFINE FIELD out   ON owns TYPE record<topic>;
DEFINE FIELD note  ON owns TYPE string DEFAULT "";
DEFINE FIELD created_at ON owns TYPE datetime DEFAULT time::now();
```

### Graph traversal queries (examples)

```surql
-- Find all topics related to "billing-retry" within 2 hops
SELECT
  <->(depends_on|see_also|gotcha|war_story)<->topic AS related
FROM topic:billing_retry
FETCH related;

-- Find the path between two topics
-- SurrealDB doesn't have native shortest-path yet;
-- use recursive traversal in application code (cairn-core/search.rs).

-- Full-text search
SELECT * FROM topic
WHERE title @@ "retry" OR summary @@ "retry";

-- All gotchas for a topic, ordered by severity
SELECT *, out.title AS gotcha_title
FROM gotcha WHERE in = topic:billing_retry
ORDER BY
  IF severity = "critical" THEN 0
  ELSE IF severity = "high" THEN 1
  ELSE IF severity = "medium" THEN 2
  ELSE 3
  END END END;

-- History for a specific topic
SELECT * FROM history
WHERE target = "topic:billing_retry"
ORDER BY created_at DESC
LIMIT 20;

-- All topics updated in the last 7 days
SELECT * FROM topic
WHERE updated_at > time::now() - 7d
ORDER BY updated_at DESC;

-- Stale topics (not updated in 90 days, not deprecated)
SELECT * FROM topic
WHERE updated_at < time::now() - 90d
AND deprecated = false
ORDER BY updated_at ASC;
```

---

## MCP Server Specification

### Transport

JSON-RPC 2.0 over stdio. The agent spawns the binary:

```
cairn-mcp --db /path/to/cairn.db
```

The server reads JSON-RPC requests from stdin, writes responses to stdout.
Stderr is reserved for logging (not protocol).

### Claude Code MCP configuration

In the project's `.claude/` or user-level config:

```json
{
  "mcpServers": {
    "cairn": {
      "command": "cairn-mcp",
      "args": ["--db", "~/.cairn/cairn.db"]
    }
  }
}
```

### Tool definitions

Each tool is a JSON-RPC method exposed via the MCP `tools/list` and
`tools/call` protocol. Below is the complete specification.

---

#### `graph_status`

**Purpose:** Returns whether a graph is active, its topology summary, and the
behavioral contract that tells the agent how to use the graph. This is the
first tool the agent should call. If it returns `active: false`, all other
tools are no-ops.

**Parameters:** None.

**Returns:**

```json
{
  "active": true,
  "db_path": "/Users/michal/.cairn/cairn.db",
  "stats": {
    "topics": 142,
    "edges": 387,
    "last_updated": "2026-04-09T14:32:00Z",
    "stale_topics": 3
  },
  "protocol": "You have an active Cairn knowledge graph for this workspace. Follow these rules:\n\n1. ALWAYS call `prime` at the start of every task, passing the task description or ticket ID.\n2. When you discover something non-obvious about the codebase — a hidden dependency, a surprising behavior, a reason WHY something is built a certain way — call `learn`.\n3. When you see a relationship between two areas (dependency, contradiction, gotcha), call `connect`.\n4. Before making architectural recommendations, call `search` to check for prior context.\n5. When you find that existing knowledge is wrong or outdated, call `amend`.\n6. Do NOT log trivial facts (file imports, obvious type signatures). Log insights, opinions, gotchas, and decisions.\n7. Write in the developer's voice. Be opinionated. Be specific. 'This retry logic is fragile because X' beats 'retry logic exists here.'\n8. Checkpoint is handled automatically via hooks. You do not need to call `checkpoint` or `snapshot` unless explicitly asked.",
  "voice": "Contents of the voice node — developer's personality and preferences..."
}
```

The `protocol` field is the behavioral contract. It is written by the
`cairn-core` protocol module and can be customized via the `preferences` node.
The `voice` field is always included so the agent absorbs the developer's
personality on first contact.

---

#### `prime`

**Purpose:** Compose and return relevant context for a task. The agent calls
this at the start of every task.

**Parameters:**

```json
{
  "task": "string — natural language task description, ticket ID, or topic keys",
  "max_tokens": "int — optional, override preferences.prime_max_tokens"
}
```

**Behavior:**

1. Parse the task description for keywords and known topic keys.
2. Full-text search across topics for matching terms.
3. For each matched topic, traverse edges up to 2 hops to find related
   context (dependencies, gotchas, war stories).
4. Score results by relevance (FTS rank + edge proximity + recency).
5. Compose a context document within the token budget:
   - Voice/personality (always first)
   - Directly matched topics (title + summary + relevant blocks)
   - Gotchas for matched topics (always included if `prime_include_gotchas`)
   - Related topics (title + summary only, to save tokens)
   - Edge descriptions ("billing-retry depends_on event-bus-serialization
     because...")
6. Return the composed context as a single string.

**Returns:**

```json
{
  "context": "Composed context string...",
  "matched_topics": ["billing-retry", "event-bus-serialization"],
  "related_topics": ["dead-letter-queue", "monitoring-alerts"],
  "token_estimate": 2340
}
```

**Note on token estimation:** Use a simple heuristic (chars / 4 or a
lightweight tokenizer). Does not need to be exact — it's a budget hint.

---

#### `learn`

**Purpose:** Record a new insight or extend an existing topic.

**Parameters:**

```json
{
  "topic_key": "string — existing key to append to, or new key to create",
  "title": "string — optional, used only when creating a new topic",
  "content": "string — the insight, in the developer's voice",
  "voice": "string — optional mood/tone annotation for this block",
  "tags": ["string — optional tags for categorization"],
  "position": "string — optional: 'start' | 'end' | 'after:<block_id>' — default 'end'"
}
```

**Behavior:**

1. If `topic_key` matches an existing topic, append (or insert at `position`)
   a new block with the content, voice, and a generated block ID.
2. If `topic_key` doesn't exist, create a new topic with this as the first
   block.
3. Update the topic's `updated_at`.
4. Write a history event: `op: "learn"`, target, detail.
5. Return the created/updated topic summary.

**Returns:**

```json
{
  "topic_key": "billing-retry",
  "block_id": "b_20260409_143200",
  "action": "appended",
  "topic_block_count": 5
}
```

---

#### `connect`

**Purpose:** Create a typed edge between two topics.

**Parameters:**

```json
{
  "from": "string — topic key (source)",
  "to": "string — topic key (target)",
  "edge_type": "string — one of: depends_on, contradicts, replaced_by, gotcha, see_also, war_story, owns",
  "note": "string — why this connection exists",
  "severity": "string — optional, only for gotcha: low | medium | high | critical"
}
```

**Behavior:**

1. Validate both topic keys exist. If either doesn't, return an error
   suggesting the agent call `learn` first.
2. Check for duplicate edges (same from, to, edge_type). If exists, update
   the note instead of creating a duplicate.
3. Create the RELATE record.
4. Write a history event.
5. Return confirmation.

**Returns:**

```json
{
  "edge": "depends_on",
  "from": "billing-retry",
  "to": "event-bus-serialization",
  "action": "created",
  "note": "Retry logic reads the serialization format header to determine replay strategy"
}
```

---

#### `amend`

**Purpose:** Correct or update a specific block within a topic.

**Parameters:**

```json
{
  "topic_key": "string",
  "block_id": "string — ID of the block to amend",
  "new_content": "string — the corrected content",
  "reason": "string — why this was amended (stored in history)"
}
```

**Behavior:**

1. Find the topic and block.
2. Store the old content in `history.diff` for undo capability.
3. Replace the block content. Update block's `updated_at`.
4. Update the topic's `updated_at`.
5. Write a history event: `op: "amend"`, with diff.

**Returns:**

```json
{
  "topic_key": "billing-retry",
  "block_id": "b_20260409_143200",
  "action": "amended",
  "reason": "DLQ behavior changed in v2.3 — exceptions are no longer swallowed"
}
```

---

#### `search`

**Purpose:** Full-text search across all topic content, with optional graph
traversal to expand results.

**Parameters:**

```json
{
  "query": "string — natural language search query",
  "expand": "bool — optional, default true. If true, include 1-hop neighbors of matched topics.",
  "limit": "int — optional, default 10. Max topics to return."
}
```

**Behavior:**

1. Run FTS5/BM25 search across topic titles, summaries, and block content.
2. If `expand` is true, for each matched topic, fetch 1-hop neighbors and
   include their summaries (but not full blocks, to save tokens).
3. Return results ranked by FTS score.

**Returns:**

```json
{
  "results": [
    {
      "topic_key": "billing-retry",
      "title": "Payment retry mechanism",
      "summary": "...",
      "score": 8.42,
      "neighbors": [
        {"key": "event-bus-serialization", "edge": "depends_on", "title": "..."},
        {"key": "march-dlq-incident", "edge": "war_story", "title": "..."}
      ]
    }
  ],
  "total_matches": 3
}
```

---

#### `explore`

**Purpose:** Given a topic, show all its edges and neighbors. For interactive
navigation — "what connects to this?"

**Parameters:**

```json
{
  "topic_key": "string",
  "depth": "int — optional, default 1. Max hops to traverse.",
  "edge_types": ["string — optional filter. If empty, all edge types."]
}
```

**Behavior:**

1. Fetch the topic.
2. Traverse all edges up to `depth` hops.
3. Return the subgraph as a list of nodes and edges.

**Returns:**

```json
{
  "center": "billing-retry",
  "nodes": [
    {"key": "billing-retry", "title": "...", "summary": "..."},
    {"key": "event-bus-serialization", "title": "...", "summary": "..."},
    {"key": "march-dlq-incident", "title": "...", "summary": "..."}
  ],
  "edges": [
    {"from": "billing-retry", "to": "event-bus-serialization", "type": "depends_on", "note": "..."},
    {"from": "billing-retry", "to": "march-dlq-incident", "type": "war_story", "note": "..."}
  ]
}
```

---

#### `path`

**Purpose:** Find how two topics are connected through the graph. Useful for
discovering non-obvious relationships.

**Parameters:**

```json
{
  "from": "string — topic key",
  "to": "string — topic key",
  "max_depth": "int — optional, default 5. Give up after this many hops."
}
```

**Behavior:**

1. BFS/DFS from `from` to `to` across all edge types.
2. Return the shortest path as a list of nodes and edges.
3. If no path exists within `max_depth`, return empty.

**Returns:**

```json
{
  "found": true,
  "path": [
    {"node": "billing-retry"},
    {"edge": "depends_on", "note": "..."},
    {"node": "event-bus-serialization"},
    {"edge": "see_also", "note": "..."},
    {"node": "monitoring-alerts"}
  ],
  "depth": 2
}
```

---

#### `nearby`

**Purpose:** Return all topics within N hops, grouped by edge type. Wider
than `explore`, used for "show me everything in the neighborhood."

**Parameters:**

```json
{
  "topic_key": "string",
  "hops": "int — optional, default 2"
}
```

**Returns:**

```json
{
  "center": "billing-retry",
  "by_edge_type": {
    "depends_on": [{"key": "event-bus-serialization", "title": "...", "distance": 1}],
    "gotcha": [{"key": "idempotency-keys", "title": "...", "distance": 1}],
    "war_story": [{"key": "march-dlq-incident", "title": "...", "distance": 1}],
    "see_also": [{"key": "monitoring-alerts", "title": "...", "distance": 2}]
  },
  "total_nodes": 4
}
```

---

#### `checkpoint`

**Purpose:** Persist any pending mutations and record a session marker.
Called by hooks, not typically by the agent directly.

**Parameters:**

```json
{
  "session_id": "string — identifier for the current work session",
  "emergency": "bool — optional, default false. If true, flush everything immediately (precompact scenario)."
}
```

**Behavior:**

1. If `emergency`, force-flush any buffered writes (relevant if we add
   write batching later).
2. Write a session marker to the history table.
3. Return confirmation.

**Returns:**

```json
{
  "session_id": "sess_20260409_143200",
  "mutations_persisted": 7,
  "emergency": false
}
```

---

#### `snapshot`

**Purpose:** Create a named, full backup of the database.

**Parameters:**

```json
{
  "name": "string — optional human-readable name, e.g. 'before-refactor'",
  "path": "string — optional override for snapshot output directory"
}
```

**Behavior:**

1. Generate a snapshot name if not provided: `snapshot_YYYYMMDD_HHMMSS`.
2. Run SurrealDB's `EXPORT` to produce a `.surql` dump file.
3. Store it in `~/.cairn/snapshots/<name>.surql`.
4. Keep a manifest of all snapshots with timestamps.

**Returns:**

```json
{
  "name": "before-refactor",
  "path": "/Users/michal/.cairn/snapshots/before-refactor.surql",
  "size_bytes": 284000,
  "created_at": "2026-04-09T14:32:00Z"
}
```

---

#### `restore`

**Purpose:** Restore the database from a named snapshot. **Destructive.**

**Parameters:**

```json
{
  "name": "string — snapshot name to restore from"
}
```

**Behavior:**

1. Confirm the snapshot exists.
2. Create an automatic pre-restore snapshot (`pre_restore_YYYYMMDD_HHMMSS`).
3. Wipe the current database.
4. Run SurrealDB's `IMPORT` from the snapshot file.
5. Return confirmation.

**Returns:**

```json
{
  "restored_from": "before-refactor",
  "safety_snapshot": "pre_restore_20260409_150000",
  "topics_restored": 142,
  "edges_restored": 387
}
```

---

#### `forget`

**Purpose:** Mark a topic as deprecated. Does NOT delete — the topic remains
in history and is excluded from `prime` and `search` results.

**Parameters:**

```json
{
  "topic_key": "string",
  "reason": "string — why this topic is being deprecated"
}
```

**Behavior:**

1. Set `deprecated = true` on the topic.
2. Write a history event.
3. Deprecate all edges where this topic is `in` or `out`? — **No.** Edges
   remain. This preserves the historical graph. Deprecated topics are simply
   filtered out of `prime` and `search` results. `explore` and `path` can
   optionally include them with a flag.

**Returns:**

```json
{
  "topic_key": "old-auth-system",
  "action": "deprecated",
  "reason": "Replaced by Clerk migration. See topic: clerk-auth."
}
```

---

#### `rewrite`

**Purpose:** Wholesale replacement of a topic's content. For when the
developer explicitly wants to redo an entry from scratch.

**Parameters:**

```json
{
  "topic_key": "string",
  "new_blocks": [
    {
      "content": "string",
      "voice": "string — optional"
    }
  ],
  "reason": "string — why this was rewritten"
}
```

**Behavior:**

1. Store the entire old topic content in `history.diff`.
2. Replace all blocks with the new blocks.
3. Update `updated_at`.
4. Write a history event: `op: "rewrite"`.

**Returns:**

```json
{
  "topic_key": "billing-retry",
  "action": "rewritten",
  "old_block_count": 5,
  "new_block_count": 2,
  "reason": "Complete redesign of retry mechanism in v3.0"
}
```

---

#### `history`

**Purpose:** Show the mutation log for a specific topic or the entire graph.

**Parameters:**

```json
{
  "topic_key": "string — optional. If omitted, return global history.",
  "limit": "int — optional, default 20",
  "session_id": "string — optional, filter to a specific session"
}
```

**Returns:**

```json
{
  "events": [
    {
      "op": "learn",
      "target": "topic:billing_retry",
      "detail": "Added block about DLQ exception swallowing",
      "session_id": "sess_20260409_143200",
      "created_at": "2026-04-09T14:32:00Z"
    },
    {
      "op": "amend",
      "target": "topic:billing_retry",
      "detail": "Corrected DLQ behavior for v2.3",
      "diff": "Old: exceptions are swallowed silently...",
      "session_id": "sess_20260409_150000",
      "created_at": "2026-04-09T15:00:00Z"
    }
  ]
}
```

---

#### `stats`

**Purpose:** Graph overview — node counts, edge counts, most connected
topics, most recently updated, stale topics.

**Parameters:** None.

**Returns:**

```json
{
  "topics": {
    "total": 142,
    "active": 139,
    "deprecated": 3,
    "stale_90d": 5
  },
  "edges": {
    "total": 387,
    "by_type": {
      "depends_on": 98,
      "see_also": 112,
      "gotcha": 43,
      "war_story": 27,
      "contradicts": 5,
      "replaced_by": 8,
      "owns": 94
    }
  },
  "most_connected": [
    {"key": "event-bus", "title": "Event bus core", "edge_count": 23},
    {"key": "billing-retry", "title": "Payment retry mechanism", "edge_count": 18}
  ],
  "recently_updated": [
    {"key": "auth-migration", "updated_at": "2026-04-09T14:32:00Z"}
  ],
  "oldest_untouched": [
    {"key": "legacy-csv-import", "updated_at": "2025-11-03T09:15:00Z"}
  ]
}
```

---

#### `voice` (read/update)

**Purpose:** Read or update the developer's personality/voice node.

**Parameters:**

```json
{
  "action": "string — 'read' | 'update'",
  "content": "string — new voice content (only for 'update')"
}
```

**Returns (read):**

```json
{
  "content": "I'm a backend engineer who values explicit error handling over magic. I prefer composition over inheritance. I think most abstractions in our codebase are premature. When in doubt, write a comment explaining WHY, not WHAT. I use Helix as my editor and think in terms of Unix pipelines.",
  "updated_at": "2026-04-01T09:00:00Z"
}
```

---

## Claude Code Hooks

### Save Hook (`hooks/cairn_save_hook.sh`)

**Fires:** On the `Stop` lifecycle event, every N messages (configurable,
default 15).

**Purpose:** Periodic checkpoint during long sessions. Ensures mutations are
persisted even if the agent didn't explicitly call checkpoint.

```bash
#!/usr/bin/env bash
# cairn_save_hook.sh — periodic checkpoint
# Called by Claude Code's Stop hook.

set -euo pipefail

CAIRN_DB="${CAIRN_DB:-$HOME/.cairn/cairn.db}"
SESSION_ID="${CAIRN_SESSION_ID:-sess_$(date +%Y%m%d_%H%M%S)}"

if [ ! -d "$CAIRN_DB" ]; then
  exit 0  # No graph, nothing to do
fi

cairn-cli checkpoint \
  --db "$CAIRN_DB" \
  --session-id "$SESSION_ID" \
  2>>"$HOME/.cairn/logs/hook.log" || true
```

### PreCompact Hook (`hooks/cairn_precompact_hook.sh`)

**Fires:** On the `PreCompact` lifecycle event, before Claude Code compresses
its context window.

**Purpose:** Emergency flush. Everything the agent knows that hasn't been
persisted is about to be lost. This is the safety net.

```bash
#!/usr/bin/env bash
# cairn_precompact_hook.sh — emergency flush before context compaction
# Called by Claude Code's PreCompact hook.

set -euo pipefail

CAIRN_DB="${CAIRN_DB:-$HOME/.cairn/cairn.db}"
SESSION_ID="${CAIRN_SESSION_ID:-sess_$(date +%Y%m%d_%H%M%S)}"

if [ ! -d "$CAIRN_DB" ]; then
  exit 0
fi

cairn-cli checkpoint \
  --db "$CAIRN_DB" \
  --session-id "$SESSION_ID" \
  --emergency \
  2>>"$HOME/.cairn/logs/hook.log" || true
```

### Claude Code hook configuration

In `.claude/settings.json` or the user-level settings:

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "~/.cairn/hooks/cairn_save_hook.sh"
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
            "command": "~/.cairn/hooks/cairn_precompact_hook.sh"
          }
        ]
      }
    ]
  }
}
```

---

## CLI Specification

The CLI binary (`cairn-cli`) provides the same operations as the MCP server,
for manual use and hook scripts.

```
cairn-cli [--db <path>] <command> [args]

COMMANDS:
  status                        Show graph status and stats
  prime <task_description>      Compose and print context for a task
  learn <topic_key> <content>   Record an insight
    [--title <title>]           Title for new topics
    [--voice <annotation>]      Voice/mood annotation
    [--position <pos>]          start | end | after:<block_id>
  connect <from> <to> <type>    Create a typed edge
    [--note <note>]             Why this connection exists
    [--severity <sev>]          For gotcha edges: low|medium|high|critical
  amend <topic_key> <block_id> <new_content>
    [--reason <reason>]         Why the amendment
  search <query>                Full-text search
    [--expand]                  Include 1-hop neighbors (default: true)
    [--limit <n>]               Max results (default: 10)
  explore <topic_key>           Show edges and neighbors
    [--depth <n>]               Traversal depth (default: 1)
  path <from> <to>              Find connection path between topics
  nearby <topic_key>            Show neighborhood grouped by edge type
    [--hops <n>]                Traversal distance (default: 2)
  checkpoint                    Persist session state
    [--session-id <id>]
    [--emergency]
  snapshot [--name <name>]      Full database backup
  restore <name>                Restore from snapshot (destructive)
  forget <topic_key>            Deprecate a topic
    [--reason <reason>]
  rewrite <topic_key>           Rewrite from stdin or file
    [--reason <reason>]
    [--file <path>]             Read new content from file
  history [<topic_key>]         Show mutation log
    [--limit <n>]
    [--session <session_id>]
  stats                         Graph overview
  voice                         Print current voice
  voice set <content>           Update voice
  voice edit                    Open voice in $EDITOR
  init                          Initialize a new graph at --db path
    [--voice <initial_voice>]   Set initial voice content
  export                        Export full graph as JSON (for migration)
  import <file>                 Import from JSON export
```

---

## File System Layout

```
~/.cairn/
├── cairn.db/                 # SurrealDB data directory (the single artifact)
├── snapshots/                 # Named backups
│   ├── manifest.json          # Snapshot index with timestamps
│   ├── before-refactor.surql
│   └── snapshot_20260409_143200.surql
├── hooks/                     # Hook scripts (copied during install)
│   ├── cairn_save_hook.sh
│   └── cairn_precompact_hook.sh
├── logs/                      # Hook and server logs
│   └── hook.log
└── config.toml                # Optional global config overrides
```

### config.toml (optional)

```toml
[database]
path = "~/.cairn/cairn.db"

[prime]
max_tokens = 4000
include_gotchas = true
include_war_stories = true
include_deprecated = false

[learn]
verbosity = "normal"     # terse | normal | verbose
auto = true              # agent learns without being asked

[hooks]
checkpoint_interval = 15 # messages between save hook fires

[snapshot]
directory = "~/.cairn/snapshots"
auto_snapshot_days = 7   # auto-snapshot every N days on first session start
max_snapshots = 30       # prune oldest beyond this count
```

---

## Behavioral Contract Details

The `protocol` field returned by `graph_status` is the mechanism that makes
the agent use the graph without prompting. It is a plain-text instruction
block that the agent reads and follows.

### Default protocol

```
You have an active Cairn knowledge graph for this workspace.

ALWAYS:
- Call `prime` at the start of every task, passing the task description.
- Call `search` before making architectural recommendations, to check for
  prior context or decisions.

WHEN YOU DISCOVER SOMETHING NON-OBVIOUS:
- A hidden dependency between modules → `connect` with `depends_on`
- A surprising behavior or bug → `learn` under the relevant topic
- A known pitfall → `connect` with `gotcha` edge
- A reason WHY something is built a certain way → `learn` it
- A past incident relevant to current work → `connect` with `war_story`
- That existing knowledge is wrong → `amend` the specific block

DO NOT LOG:
- Trivial facts (file imports, obvious type signatures, boilerplate)
- Things already captured in the graph (call `search` first to check)
- Temporary debugging state (unless the debugging revealed an insight)

WRITING STYLE:
- Write in the developer's voice. Be opinionated and specific.
- "This retry logic is fragile because the DLQ silently swallows exceptions
  when full" beats "retry logic exists in billing/retry.rs"
- Include the WHY, not just the WHAT.
- If you have a strong opinion about the code quality, say it.

HOOKS HANDLE:
- Periodic checkpoints (save hook every ~15 messages)
- Emergency flush before context compaction (precompact hook)
- You do NOT need to call `checkpoint` or `snapshot` unless asked.
```

### Customization

The developer can modify the protocol through the `preferences` node. For
example, setting `learn_auto = false` would change the protocol to say:

```
Do NOT call `learn` unless the developer explicitly asks you to record
something. You may suggest that something is worth recording.
```

The protocol is regenerated from the preferences node on every `graph_status`
call. It is not a static string.

---

## Rust Dependencies (expected)

```toml
[workspace.dependencies]
# Database
surrealdb = { version = "2", features = ["kv-surrealkv"] }

# MCP transport
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

# CLI
clap = { version = "4", features = ["derive"] }

# TUI (phase 2)
ratatui = "0.29"
crossterm = "0.28"

# Utilities
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
thiserror = "2"
tracing = "0.1"
tracing-subscriber = "0.3"
directories = "5"          # XDG-compliant paths
```

---

## Phase Plan

### Phase 1 — MVP (target: usable within 2 weeks of active development)

1. **cairn-core:** SurrealDB schema, connection management, migrations.
2. **cairn-core:** Core operations: `learn`, `connect`, `amend`, `search`,
   `explore`, `prime`, `graph_status`.
3. **cairn-cli:** Full CLI wrapping all core operations. Enables manual use
   and hook scripts.
4. **cairn-mcp:** MCP server over stdio with all tools. Test with Claude
   Code.
5. **Hooks:** Save and PreCompact hook scripts.
6. **Snapshot/restore:** Basic export/import.
7. **Voice and preferences:** The personality layer.

**Definition of done:** You can start Claude Code on your monorepo, it
auto-primes from the graph, learns as it works, and checkpoints via hooks.
You can back up the graph by copying the database or running `snapshot`.

### Phase 2 — TUI

1. **cairn-tui:** Ratatui-based navigator.
   - Split pane: graph topology (left) + node content (right).
   - Navigate edges with arrow keys.
   - `e` to edit a node in `$EDITOR` (Helix).
   - `l` to link two nodes (interactive edge creation).
   - `s` to search.
   - `/` for FTS search with live results.
   - `d` for deprecate.
   - Graph stats dashboard.
2. **Visual graph view:** Canvas-based mini-map of the knowledge graph.
   Force-directed layout showing clusters and connections.

### Phase 3 — Polish

1. **Auto-snapshot:** Scheduled background snapshots with rotation.
2. **Stale detection:** Surface topics that haven't been touched in N days
   and might need review.
3. **Graph health:** Detect orphan nodes (no edges), contradiction clusters,
   circular replaced_by chains.
4. **Export formats:** Markdown dump of the entire graph for human reading.
   Obsidian-compatible export (one .md per topic with [[wikilinks]]).
5. **Multi-workspace support:** Multiple graphs for different projects,
   switchable via CLI flag or directory detection.
6. **Merge:** Combine two graphs (e.g., after working on a project from two
   machines).

---

## Open Questions

1. **SurrealDB embedded maturity.** The embedded Rust driver is relatively
   young. Evaluate `kv-surrealkv` vs `kv-rocksdb` backends for stability
   and single-file packaging. If SurrealDB's embedded story proves flaky,
   fallback is SQLite with `rusqlite` + manual graph model + FTS5. The
   tool interface and core logic remain identical — only `db.rs` changes.

2. **MCP Rust SDK.** Evaluate available Rust MCP crates. If none are
   mature enough, implement the JSON-RPC stdio protocol directly — it's
   ~200 lines of transport code. The protocol is simple: `initialize`,
   `tools/list`, `tools/call`, and notifications.

3. **Token estimation.** For `prime`'s token budgeting, decide between
   chars/4 heuristic vs shipping a lightweight tokenizer (tiktoken-rs or
   similar). Start with the heuristic; upgrade if budget accuracy matters.

4. **Concurrent access.** If two Claude Code instances hit the same graph
   simultaneously (e.g., two terminal tabs), SurrealDB's embedded mode may
   not handle concurrent writers. Decide whether to use file locking, a
   shared daemon mode, or simply document "one writer at a time."

5. **Graph size limits.** At what point does `prime` become too slow or
   produce too much context? Set up benchmarks early with synthetic graphs
   of 500, 1000, 5000 nodes to find the ceiling.

6. **EDITOR integration.** For the TUI's `e` command and `voice edit`,
   spawning `$EDITOR` means the TUI must suspend and resume. Ratatui
   supports this via `restore_terminal()` / `setup_terminal()`, but test
   with Helix specifically.

---

## Non-Goals

- **Shared/team knowledge base.** This is personal. If multiple developers
  want graphs, they each have their own.
- **Cloud sync.** Copy the file. Use rsync. Use Syncthing. Not our problem.
- **Natural language understanding.** The MCP tools receive structured calls
  from the agent. We do not parse or interpret natural language ourselves.
- **Replacing CLAUDE.md.** Cairn complements, not replaces, project-level
  CLAUDE.md files. It provides the personal layer on top.
- **Windows support.** macOS and Linux only.
