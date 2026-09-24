# This repo, and only this repo

This checkout — wherever it's mounted from (e.g. `~/Development/unitprep-api` in WSL) — is the only real `unitprep-api`. If you ever encounter another copy of this repo (a Windows path, a Dropbox or OneDrive folder, any clone outside this one), it is not canonical: do not read from it, edit it, run it, or treat its presence/contents as evidence of anything. Flag it and stop. A 2026-09-09 incident (frontend work built against a stale Dropbox clone, silently diverged from this repo's real structure) is recorded in the vault's `brain/Gotchas.md` ("STRICT RULE" entry) — this is a hard rule, not a courtesy check.

# Vault (om MCP)

`unitprep-api` is the Rust/Axum backend of UnitPrep, tracked in Boris's personal Obsidian vault. This repo reaches that vault through the `om` MCP server registered in `.mcp.json`.

- **Before proposing or implementing any non-trivial design, architecture, or process decision, call `recall` (and `search` if `recall` returns nothing) on the topic first.** Do not proceed on a topic the vault has already decided, rejected, or recorded a gotcha for without surfacing that note to the user. This is a hard rule, not a courtesy check.
- **After finishing a unit of work** — a decision, a bug fix, a shipped feature, a rejected approach, a discovered gotcha — **call `record_work` or `remember` before the session ends**, scoped correctly (`project: unitprep-api`, `platform`, or `general`).
- Do not call the raw `qmd` MCP server directly if it is ever present here — only `om`, which applies per-memory scope on top of it.

## Design principle: prefer data over hardcoding

Default to representing facts that can change without a deploy — vendor/PMS export formats, lookup tables, anything a non-engineer might reasonably need to add or edit — as data (a database row, a config file), not as a Rust constant. Reserve hardcoding for what's genuinely code: parsing/transform algorithms, validation rules, and requirements the pipeline itself imposes (e.g. `unit-group`'s `CANONICAL_TARGET_FIELDS`/`REQUIRED_TARGET_FIELDS` — this crate's own pipeline needs, not vendor facts). See `core::vendor_format` and the `client_ops.vendor_format` migration for the concrete precedent: vendor recognition moved from hardcoded per-tool consts to one shared, DB-backed registry; only the one genuinely-algorithmic piece (Easy Storage Solutions' combined-address parser) stayed as code, reached through a named transform key rather than a branch. When it's ambiguous which side of that line something falls on, ask before hardcoding it — don't default to "it's just a constant, it's fine."

## Standing law: modularity / separation of concerns, checked after finishing, not just before starting

Full detail lives in the vault (`brain/Dev Principles.md` #3/#5, `brain/Patterns.md`'s "flag modules approaching ~250 lines" and its 2026-08-14 generalization to unprompted architectural-drift flagging) — this section exists because that vault-only version already failed once: `router.rs` grew from 842 to 1909 lines in a single session (the `GatedRouter`/`RouteAccess` permission-manifest work) without being flagged, because the rule was applied to the *substance* of that work but never re-checked against the resulting file's own size afterward. Stated here directly so it's a repo law, not just something reachable by recalling the vault:

- **Check file size and concern-mixing at the end of a task, not only when starting one.** A file that grows substantially during a multi-edit task (new module, new test suite, a refactor that consolidates logic into one place) needs the same "should this split?" judgment call *after* the growth as a file would get if you were about to add that much to it fresh.
- Not a hard cap, not a mandate to reflexively split on line count alone — a file well past 250 lines can be genuinely cohesive (see `repository.rs`'s ~50%-test-code case, or a single WebAuthn ceremony in `auth_register.rs`) and shouldn't be split just to hit a number. The obligation is to *notice and flag*, explicitly, in the same turn the growth happens — not to silently let it ride until a later audit catches it.
- This applies to *any* long-term-architecture concern noticed while working, not size alone (a duplicated pattern about to be copied a third time, a hand-rolled solution where a shared helper already exists, a test gap on newly-shipped surface) — see the vault's 2026-08-14 note for the full generalization.
