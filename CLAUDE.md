# Vault (om MCP)

`unitprep-api` is the Rust/Axum backend of UnitPrep, tracked in Boris's personal Obsidian vault. This repo reaches that vault through the `om` MCP server registered in `.mcp.json`.

- **Before proposing or implementing any non-trivial design, architecture, or process decision, call `recall` (and `search` if `recall` returns nothing) on the topic first.** Do not proceed on a topic the vault has already decided, rejected, or recorded a gotcha for without surfacing that note to the user. This is a hard rule, not a courtesy check.
- **After finishing a unit of work** — a decision, a bug fix, a shipped feature, a rejected approach, a discovered gotcha — **call `record_work` or `remember` before the session ends**, scoped correctly (`project: unitprep-api`, `platform`, or `general`).
- Do not call the raw `qmd` MCP server directly if it is ever present here — only `om`, which applies per-memory scope on top of it.

## Design principle: prefer data over hardcoding

Default to representing facts that can change without a deploy — vendor/PMS export formats, lookup tables, anything a non-engineer might reasonably need to add or edit — as data (a database row, a config file), not as a Rust constant. Reserve hardcoding for what's genuinely code: parsing/transform algorithms, validation rules, and requirements the pipeline itself imposes (e.g. `unit-group`'s `CANONICAL_TARGET_FIELDS`/`REQUIRED_TARGET_FIELDS` — this crate's own pipeline needs, not vendor facts). See `core::vendor_format` and the `client_ops.vendor_format` migration for the concrete precedent: vendor recognition moved from hardcoded per-tool consts to one shared, DB-backed registry; only the one genuinely-algorithmic piece (Easy Storage Solutions' combined-address parser) stayed as code, reached through a named transform key rather than a branch. When it's ambiguous which side of that line something falls on, ask before hardcoding it — don't default to "it's just a constant, it's fine."
