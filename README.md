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

### Duplicate tenant check request flow

A separate, independent tool and session type, tracked the same way
(`session_id`, same idle timeout). No correction loop — this tool's
whole job is to identify and list inconsistencies; corrections are made
by the client the report is prepared for, outside the platform:

1. `POST /dedup/check` — multipart upload of one QMS End Users export
   CSV. Ingests and runs the full check synchronously (no ambiguity to
   resolve first, unlike UnitGroup's multi-file/group-file-selection
   flow), creates a session, and returns `{session_id, report}` — every
   multi-unit tenant with a contact-info mismatch (grouped by exact
   `FirtLast` match), and every typo/name-variant candidate surfaced for
   human confirmation (never auto-merged, regardless of similarity
   score).
2. `POST /dedup/report` — re-fetches the same report by `session_id`
   (e.g. after a page refresh), without re-uploading the file.
3. `POST /dedup/export` — the same report as a downloadable CSV:
   flagged groups first (one row per record, note on each group's first
   row), followed by a typo/name-variant section.

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
  Includes both UnitGroup's endpoints and the duplicate-tenant-check
  tool's (`/dedup/check`, `/dedup/report`, `/dedup/export`).
- `src/application/` — session orchestration, one file per tool:
  `session_service.rs` (UnitGroup — parses uploads into a `Session`) and
  `dedup_session_service.rs` (duplicate-tenant-check — parses, ingests,
  and analyzes a QMS export into a `DedupSession`). The generic storage
  mechanics both build on (`SessionStore` trait, `InMemorySessionStore`)
  live in `unitprep-core`, not here.
- `src/domain/` — UnitGroup's own business logic: discovery/validation
  rules, the analysis/fingerprint-matching engine, domain models. File
  parsing itself (CSV/XLSX/SpreadsheetML) also moved to `unitprep-core`
  (`core/src/parsing/`), since it's identical regardless of which tool
  is consuming the data. The duplicate-tenant-check tool's own domain
  logic lives entirely in `dedup/`, not here — see below.
- `src/infrastructure/` — export artifact generation: `csv_export.rs`
  (UnitGroup — CSV/JSON/ZIP) and `dedup_csv_export.rs`
  (duplicate-tenant-check — CSV).
- `src/ai/` — placeholder seam for future AI-assisted decision support;
  not wired into the pipeline yet. (A more concrete version of this idea
  already exists for one specific case — see `dedup/`'s `NoteComposer`
  trait below.)
- `core/` — the `unitprep-core` crate: `parsing/` (per-format parsers),
  `csv_document.rs`/`uploaded_file.rs` (source-agnostic document models),
  `session.rs`/`session_store.rs`/`in_memory_session_store.rs` (the
  generic session engine, generic over any tool's own session type).
- `unit-group/` — an intentionally empty crate stub; the eventual home
  for this binary's domain logic once it's extracted out, not yet done.
- `dedup/` — the `unitprep-dedup` crate: the duplicate-tenant-check
  tool's domain logic (grouping, contact-info comparison, note
  composition, typo/name-variant detection), depending only on
  `unitprep-core`. No session state, HTTP, or export format — those are
  the binary's job, wired up in `src/application/dedup_session_service.rs`,
  `src/api/dedup.rs`, and `src/infrastructure/dedup_csv_export.rs`.

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
