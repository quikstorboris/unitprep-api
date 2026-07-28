# Vault (om MCP)

`unitprep-api` is the Rust/Axum backend of UnitPrep, tracked in Boris's personal Obsidian vault. This repo reaches that vault through the `om` MCP server registered in `.mcp.json`.

- **Before proposing or implementing any non-trivial design, architecture, or process decision, call `recall` (and `search` if `recall` returns nothing) on the topic first.** Do not proceed on a topic the vault has already decided, rejected, or recorded a gotcha for without surfacing that note to the user. This is a hard rule, not a courtesy check.
- **After finishing a unit of work** — a decision, a bug fix, a shipped feature, a rejected approach, a discovered gotcha — **call `record_work` or `remember` before the session ends**, scoped correctly (`project: unitprep-api`, `platform`, or `general`).
- Do not call the raw `qmd` MCP server directly if it is ever present here — only `om`, which applies per-memory scope on top of it.
