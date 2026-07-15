# unitprep-api

UnitPrep's backend: a Rust/Axum HTTP API for storage-facility unit-import
preparation. It compares UnitGroup names discovered in facility unit
exports against a master/reference Unit Group file, identifies net-new
groups, flags advisory (non-authoritative) similarity warnings, and
generates a downloadable ZIP of migration-ready import artifacts.

This project has no CLI — it is a session-oriented web service. The
frontend is [`unitprep-ui`](../unitprep-ui) (Next.js).

## Running

```bash
cargo run
```

Starts the API on `http://0.0.0.0:8080` (reachable at `127.0.0.1:8080`
locally). Override with the `HOST`/`PORT` env vars if needed — most
hosting platforms (Fly.io, Render, etc.) inject `PORT` automatically.

For anything performance-sensitive, run the optimized build instead —
`cargo run --release` (or build once with `cargo build --release` and
execute `target/release/unitprep` directly). The dev profile is
meaningfully slower for CPU-bound work like Excel parsing; this is a
deploy-time decision, not something toggled at runtime.

CORS defaults to `http://localhost:3000` and `http://localhost:5173`
(the frontend dev servers). Set `CORS_ALLOWED_ORIGINS` (comma-separated)
to allow real deployed frontend origins instead.

## Request flow

Each browser session is tracked server-side by `session_id` (in-memory,
10-minute idle timeout by default — override with `SESSION_TIMEOUT_SECS`).
The pipeline is sequential:

1. `POST /upload` — multipart upload of a folder's files. Creates a
   session and parses every `.csv`, `.xlsx`, and `.xls` file (including
   Excel 2003 SpreadsheetML XML mislabeled with a `.xls` extension) into
   a `CsvDocument`, returns `session_id`.
2. `POST /discover` — classifies the session's parsed documents into unit
   files (have `UnitGroup`/`Number`/`Category` columns) and master group
   files (have `Name`/`Description`/`AssignedTo`/`Status`/`LastUpdated`
   columns).
3. `POST /group-file/select` — required only when discovery finds more
   than one candidate master group file; picks the authoritative one.
4. `POST /validate` — checks discovered unit files for blank/suspicious
   `UnitGroup` values, malformed dimensions, climate/locality/dimension
   mismatches against the `UnitGroup` name, duplicate unit numbers,
   inconsistent casing, and rare/single-unit groups. Each issue reports
   the specific affected unit ids, not just a count, and (where a single
   value can fix it) which columns are correctable.
   - `POST /correct` — applies one corrected cell value (e.g. a unit's
     Width) as a session-level overlay on top of the parsed data and
     re-runs validation. The original upload is never mutated.
   - `POST /exempt-dimensions` — marks a unit as intentionally
     non-dimensioned (an office, an owner's apartment, etc. in the
     catalog) so blank Width/Length stops being flagged for it, without
     fabricating values.
   - Export is blocked while Severity::Error issues remain, unless the
     caller explicitly sets `acknowledge_errors: true` on `/export`.
5. `POST /analyze` — compares each facility's UnitGroup names against the
   selected master file. Existence is decided by **exact name match
   only**; fuzzy (fingerprint + normalized Levenshtein) similarity is
   advisory-only and never affects net-new determination.
6. `POST /export` — requires validation and analysis to have completed;
   generates net-new-groups CSV, facility/group assignment CSVs, advisory
   reports, and a `batch_run.json`, and streams them back as a ZIP built
   entirely in memory (no disk I/O, no export-folder cleanup).

`GET /health` returns a liveness check.

Every endpoint that looks up a session by id returns a distinct
`404 Session not found or expired` when it doesn't exist (expired via
the 10-minute idle timeout, or an invalid id) — never a fake zero-value
success, since those are different situations and the frontend needs to
tell them apart.

## Current security posture

**No authentication or authorization exists on any endpoint.** Any
client that can reach this API can create, read, correct, and export any
session if it has (or guesses) the `session_id`. Session ids are random
UUIDs, so this isn't trivially exploitable, but it is not a security
boundary. This is an accepted, deliberate gap for the current internal,
single-operator usage pattern — not an oversight — but it needs to be
closed before this is exposed beyond that.

## Project layout

This is a Cargo workspace, not a single crate — `unitprep-core` holds the
tool-agnostic engine (file ingestion/parsing, session storage) that any
future UnitPrep tool depends on; this binary holds the UnitGroup-specific
domain logic and HTTP layer. See `Cargo.toml`'s own comments for the
rationale.

- `src/main.rs` — process entry point, logging setup, server bind.
- `src/api/` — Axum handlers and routing, one module per endpoint.
- `src/application/` — `SessionService`, the one piece of session
  orchestration specific to this tool (parses uploads into a UnitGroup
  `Session`). The generic storage mechanics it builds on
  (`SessionStore` trait, `InMemorySessionStore`) live in `unitprep-core`,
  not here.
- `src/domain/` — business logic: discovery/validation rules, the
  analysis/fingerprint-matching engine, domain models. File
  parsing itself (CSV/XLSX/SpreadsheetML) also moved to `unitprep-core`
  (`core/src/parsing/`), since it's identical regardless of which tool
  is consuming the data.
- `src/infrastructure/` — export artifact generation (CSV/JSON/ZIP).
- `src/ai/` — placeholder seam for future AI-assisted decision support;
  not wired into the pipeline yet.
- `core/` — the `unitprep-core` crate: `parsing/` (per-format parsers),
  `csv_document.rs`/`uploaded_file.rs` (source-agnostic document models),
  `session.rs`/`session_store.rs`/`in_memory_session_store.rs` (the
  generic session engine, generic over any tool's own session type).
- `unit-group/` — an intentionally empty crate stub; the eventual home
  for this binary's domain logic once it's extracted out, not yet done.

## Tests

```bash
cargo test
```

Two layers of coverage:

- Domain-level unit tests alongside the logic they cover — heaviest on
  the fingerprint-matching engine (`src/domain/analysis/fingerprint.rs`),
  since every false-positive bug this project has hit came from two
  structurally different groups (by dimensions, location, climate, or
  area code) being fuzzy-matched as the same group.
- Endpoint-level tests in each `src/api/*.rs` module, calling handlers
  directly (`handler(State(state), Json(request)).await`) against a
  session built via the helpers in `src/api/mod.rs`'s `test_support`
  module — covering the session-not-found 404 behavior, the
  correction/exemption re-validation flow, and the export
  acknowledge-override, without needing a live server or fabricated
  multipart bodies.
