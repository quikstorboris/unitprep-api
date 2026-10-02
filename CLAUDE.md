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

## Standing law: performance is checked at realistic size, in the build you actually run

Added 2026-10-01 after a dedup check went from under a second to ~9 s. Docker was blamed; it was not the cause. The dev server (`cargo run` in the dev container) and `cargo test` are **debug builds**, ~10x slower than `--release` on allocation-heavy loops, and they exposed a pairwise pass over tenants that had always been quadratic. Full detail is in the vault (`brain/Gotchas.md`, "The Docker dev server runs a DEBUG build..."). The rules:

- **Any pass that compares every pair of tenants/units (or is otherwise O(n^2)) needs two things before it ships**: per-item data prepared once outside the pair loop, and an *exact* cheap upper-bound prune ahead of the expensive comparison (see `unitprep-dedup`'s `similarity.rs`), with a test proving the pruned result equals the brute-force result.
- **Never `await` inside a loop over N external calls** (Dropbox, HTTP) unless the concurrency is bounded and deliberate: use `futures::stream::...buffered(N)` (see `dedup_files.rs`).
- **Test a new flow at a realistic size, with a time budget.** Unit fixtures of a handful of records cannot expose quadratic cost. `dedup/src/performance_tests.rs` is the template: a deterministic synthetic facility (no real data) run through the whole pipeline in a debug build, failing over a budget set well above today's cost. When a new heavy pass is added, extend it.
- **When something "got slow", benchmark the same code natively in both profiles before blaming the container** (`cargo test --release` vs plain), then profile by stage.
- `src/api/slow_operation.rs` logs a WARN when a dedup check or folder scan exceeds 2 s; wire new potentially-slow handlers into `warn_if_slow` rather than relying on someone noticing.
- `Cargo.toml` optimizes the dedup/core/unit-group crates and the csv/calamine parsers in the dev profile so debug runs stay near production speed; keep new hot-path crates on that list.


# Tech stack list (standing rule)

Added 2026-10-01. The vault note `reference/UnitPrep Tech Stack.md` is the canonical list of every tool, library, service and infrastructure piece used across both repos (name, area, what/why). **Whenever you introduce a new one in this repo — a Cargo/npm dependency, CI/security tool, external service or integration, infra component — add a row to that note in the same session and bump its "Last updated" line.** Also update the row if a tool is removed or replaced. Boris uses that note for CTO/architecture presentations, so it must not go stale.
