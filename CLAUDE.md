# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Cairn is a personal, out-of-repo knowledge graph that an AI coding agent uses to accumulate and retrieve codebase understanding over time. It stores topics, edges, gotchas, and war stories in a SurrealDB-backed graph, exposed via MCP tools and a CLI. See `DESIGN.md` for the full specification.

## Architecture

Rust workspace with four crates, three binaries (TUI deferred to phase 2):

- **cairn-core/** — Library crate containing all graph logic: SurrealDB schema/migrations (`db.rs`), semantic operations (`ops.rs`), context composition (`prime.rs`), search (`search.rs`), backup/restore (`snapshot.rs`), behavioral contract generation (`protocol.rs`), types (`types.rs`), errors (`error.rs`)
- **cairn-mcp/** — MCP server binary. Thin JSON-RPC stdio transport dispatching to cairn-core. No logic duplication.
- **cairn-cli/** — CLI binary (clap-based). Same operations as MCP, used for manual use and hook scripts.
- **cairn-tui/** — TUI binary (ratatui). Phase 2 placeholder.
- **hooks/** — Shell scripts for Claude Code integration (save hook, precompact hook)

All binaries are thin dispatchers into cairn-core. Logic lives in the library crate.

## Build & Development Commands

```bash
cargo build                    # build all crates
cargo build -p cairn-core      # build just the library
cargo build -p cairn-mcp       # build just the MCP server
cargo build -p cairn-cli       # build just the CLI
cargo test                     # run all tests
cargo test -p cairn-core       # test just the library
cargo test -- test_name        # run a single test
cargo clippy --all-targets     # lint
cargo fmt --check              # check formatting
cargo fmt                      # auto-format
```

## Key Design Decisions

- **SurrealDB embedded mode** — in-process, no server, no network. Uses `surrealdb` crate with `kv-surrealkv` feature. Single database directory on disk.
- **Semantic tools, not CRUD** — MCP tools are `learn`, `connect`, `amend`, `prime`, `search`, `explore`, etc. Never "create node" or "insert edge."
- **Zero-prompt operation** — `graph_status` returns a behavioral contract (`protocol` field) that tells the agent how to use the graph automatically.
- **Soft deletes** — `forget` marks topics as deprecated rather than deleting. Edges remain for historical graph preservation.
- **History/audit trail** — Every mutation is recorded in a `history` table with optional diffs for undo capability.
- **Token budgeting in `prime`** — Uses chars/4 heuristic for token estimation.
- **Facade pattern** — `Cairn` struct in `lib.rs` wraps `CairnDb` and exposes all operations as methods. CLI and MCP both dispatch through this facade.
- **MCP via rmcp** — Uses the official `rmcp` crate (v1.3) with `#[tool_router]`/`#[tool]` macros for tool registration. Stdio transport.

## SurrealDB Gotchas

These were discovered during implementation and are important for future work:

- Edge tables must use `DEFINE TABLE ... TYPE RELATION SCHEMAFULL`, not just `SCHEMAFULL`
- Nested objects in `array<object>` fields have issues with `id` fields (SurrealDB treats `id` as special). Blocks are stored as a JSON string (`TYPE string`) instead.
- FTS (BM25) indexes must be one per field — a single index on `(title, summary)` doesn't work. Use separate indexes with different `@N@` references.
- `SELECT *` returns SurrealDB's internal `id` field which may not deserialize cleanly. Select specific fields instead.
- Record IDs from `SELECT VALUE id` return `surrealdb::sql::Thing`; use `.to_string()` for query interpolation.

## Target Platforms

macOS (Apple Silicon and Intel) and Linux (x86_64, aarch64). Windows is explicitly out of scope.

## Data Location

Runtime data lives at `~/.cairn/` — database, snapshots, hooks, logs, and optional `config.toml`. The database path is configurable via `--db` flag or `CAIRN_DB` env var.

## SurrealDB Fallback

If SurrealDB embedded proves unstable, the fallback is SQLite with `rusqlite` + manual graph model + FTS5. Only `db.rs` changes; tool interface and core logic stay identical.
