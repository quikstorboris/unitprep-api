# Changelog

All notable changes to `unitprep-api` are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Postgres connectivity via sqlx, connecting as a dedicated
  app_service role rather than the migration/owner role, so row-level
  security actually applies to application traffic. DATABASE_URL
  configures the connection pool, built lazily so a missing or
  incorrect credential does not block application startup. GET
  /health/db reports connectivity and confirms which role the pool is
  actually authenticating as.
- AuthBackend trait plus a webauthn-rs-backed implementation
  (WebauthnRsBackend), stored in AppState behind Arc<dyn ...> the same
  way the existing session stores are -- no HTTP endpoints call it yet,
  this is the interface and one implementation behind it, per the
  standing interface-first design rule.
- Session cookie plumbing: opaque token generation and hashing
  (session_token.rs) and httpOnly/Secure/SameSite cookie issuance,
  reading, and clearing (session_cookie.rs). Deliberately unsigned and
  unencrypted -- the cookie carries an opaque random token that is only
  ever trusted after a database round-trip, never decoded as a claim.
- AuthenticatedUser, an axum extractor resolving the session cookie
  into a verified user id and role via resolve_session(), plus
  begin_rls_transaction for handlers that need to run further
  RLS-scoped queries under that identity. GET /health/whoami exercises
  the whole chain end to end for now, since no real protected endpoint
  exists yet to exercise it through.

## [1.1.2] - 2026-07-28

Test coverage and two crash fixes found through it; no new functionality.

### Fixed
- `cell_to_string` (Excel parsing): a date-typed cell with an extreme
  serial number could panic inside chrono's `TimeDelta` construction,
  not just return `None` as calamine's own doc comment claims. Wrapped
  in `catch_unwind`, falling back to the raw serial number for that one
  cell instead of failing the whole request.
- SpreadsheetML parsing: `ss:Index`/`ss:MergeAcross` were parsed from
  untrusted XML into `usize` with no bound, then fed straight to
  `Vec::resize` -- a single crafted cell (e.g. `ss:Index="99999999999999"`)
  could attempt an astronomical allocation and abort the whole process,
  not a `panic!` the catch-panic middleware could intercept. Both
  attributes are now clamped to `1..=16384` (Excel's own real column
  limit) at the point they're parsed.

### Added
- Property-based/fuzz tests (via `proptest`) for all three file
  parsers -- the two fixes above were both found this way.
- Real HTTP-level integration tests (`src/api/http_integration_tests.rs`):
  bind the actual router to a loopback port and drive it with a real
  `reqwest` client, including an automated regression test for the CORS
  credentials fix (previously verified only by hand in a browser).
- Regression tests for the analyze/export session write-back race.
- `cargo-llvm-cov` wired in as the primary coverage tool (`cargo cov`
  / `cargo cov-summary` aliases in `.cargo/config.toml`) -- current
  baseline 84% lines workspace-wide. `cargo-tarpaulin` also available
  (`cargo cov-tarpaulin`) as an occasional independent cross-check, not
  a second tool to run routinely alongside llvm-cov.

## [1.1.1] - 2026-07-28

No new functionality; a full post-1.1.0 hygiene, security, and
correctness pass across every crate.

### Added
- `SessionMetadata.owner_id: Option<Uuid>` plus owner-gated
  `with_owned_session`/`with_owned_session_mut` store lookups, threaded
  through both session-creating handlers (currently passing `None` --
  no `AuthenticatedUser` exists on either yet).
- Router-wide panic-catching middleware
  (`tower_http::catch_panic::CatchPanicLayer`) returning the project's
  own `ApiErrorBody` 500 shape instead of dropping the connection.
- First test coverage for the upload handler (4 tests via a real
  multipart request).
- A synthetic full-pipeline dedup test covering grouping, flagging,
  typo-variant detection, and relatedness together on fabricated data.
- `.cargo/audit.toml` documenting one accepted, non-reachable
  `cargo-audit` finding (`RUSTSEC-2023-0071`, an optional `sqlx-mysql`
  dependency never compiled into this binary).

### Changed
- Applied `cargo fmt` across the entire workspace (no rustfmt.toml
  existed before this; formatting was never mechanized).
- `unit-group`: removed unnecessary clones in `analyze_batch`, added a
  group-fingerprint cache to `RowScan`, indexed `apply_corrections`
  lookups by unit instead of rescanning per row, removed dead/lossy
  code in `models.rs` and consolidated on a single public type name for
  advisory issues.
- `dedup`: fixed 4 blank-vs-normalized-value comparison/display bugs
  across `comparison.rs`/`phrasing.rs`, an address-join bug in
  `relatedness.rs` that could collapse two different addresses into
  one string, and a title-casing bug for names like `O'Brien`.
- `csv_export.rs` and the dedup CSV/XLSX writers now share single
  helpers (`write_csv`, `record_field_values`) instead of each
  independently repeating the same boilerplate/field list.
- `DiscoverResponse::from(&DiscoveryResult)` replaces ~60 lines of
  hand-copying with ~15; `Session::effective_documents_for(names)`
  lets `analyze`/`validate`/discovery filter to relevant documents
  before the mapping/correction/exclusion transform instead of after;
  `AnalysisResults` is now `Arc`-wrapped so passing it around a session
  is a refcount bump instead of a deep clone.
- Bumped `quick-xml` (0.36 -> 0.41, direct and via `calamine`) and
  `calamine` (0.25 -> 0.36) for two RustSec advisories reachable via
  uploaded SpreadsheetML files.

### Fixed
- A real CORS gap: the frontend's shared fetch hooks send
  `credentials: "include"`, but the API's `CorsLayer` didn't set
  `Access-Control-Allow-Credentials: true` -- which per the Fetch/CORS
  spec makes a credentialed response invisible to the browser
  regardless of whether a cookie exists yet. Verified live against a
  running frontend, not just by reading code.

## [1.1.0] - 2026-07-20

### Added
- Duplicate tenant check — a second, independent tool: `unitprep-dedup`
  (new workspace crate — grouping/comparison/typo-variant domain logic,
  depending only on `unitprep-core`, no session/HTTP/export concerns)
  plus its own session type and three endpoints, `POST /dedup/check`,
  `POST /dedup/report`, `POST /dedup/export`. Every typo/name-variant
  candidate is surfaced for human confirmation, never auto-merged.
  Domain logic verified against real facility exports, byte-for-byte
  matching an independently-confirmed reference-script run on one of
  them.
- CSV parsing now tolerates a trailing unnamed column beyond the
  header's last field (a real, consistent quirk in some facility
  export tools) instead of rejecting every row of an affected file.
- Startup log now includes the process's PID, so a specific running
  instance can be identified from its own log output without a
  separate `ps`/`ss` lookup.

### Changed
- UnitGroup's own domain logic (discovery-result/validation-result
  data, batch building, fingerprint matching, validation rules,
  correction overlays) moved out of the binary's `src/domain/` into the
  previously-empty `unitprep-unit-group` crate — the same
  domain/session boundary `unitprep-dedup` established, applied back to
  the original tool. `Session`/`WorkflowStage`/`StageError` (the stage
  machine) stay in the binary, in `src/application/unit_group_session.rs`.
  No behavior change — verified via the full existing test suite (moved
  intact, none lost) and a live run of the full
  upload/discover/validate/analyze/export pipeline.
- Calling an endpoint before the session has reached the required
  workflow stage (e.g. `/analyze` before `/validate`) now returns
  `409 Conflict` with a structured `{ error, message }` body, instead of
  a fake all-zero `200` success that looked identical to a real,
  successful "nothing to report" result. Every error response across
  the API now shares this same `{ error, message }` shape.
- `POST /group-file/select` now returns the same structured error shape
  as the rest of the API instead of a `200` with `{ success: false }`:
  `409 Conflict` if called before discovery has completed, `400 Bad
  Request` (`group_file_invalid`) if the named file wasn't one
  discovery actually found.
- `POST /session/cancel` stays intentionally idempotent (always `200`,
  even for an unknown session id — that's not an error worth surfacing)
  but its response now includes `deleted: bool`, so a caller that does
  care can tell "deleted a real session" apart from "there was nothing
  there," without changing the success contract.

### Fixed
- `/discover` no longer gets permanently stuck when zero master group
  files are found — the exact shape of a net-new client with nothing
  in QMS yet to cross-reference against. `ready` previously required
  `group_files.len() == 1`, so zero candidates was treated the same as
  "ambiguous, needs selection," except with no candidates to select
  from — a real dead end with no way to proceed. Analysis already
  handled a missing reference set correctly (every discovered group
  becomes net-new); only the discovery-readiness gate was wrong. Zero
  or one candidate is now ready; only *more than one* still requires
  `/group-file/select`. `DiscoverResponse` also now includes
  `discovered_group_names` — the distinct UnitGroup values found across
  the discovered unit files (reusing `build_batch_from_documents`) — so
  the UI can show the user what was actually found before they commit
  to validate/export, most useful exactly when there's no master file
  to cross-check against yet.
- Starting a second instance against an already-bound port used to
  panic with a bare "Address already in use" and no next step. It now
  exits cleanly with a message pointing at the command to find the
  other process (`ss -ltnp | grep :PORT` or `lsof -i :PORT`) — the
  actually useful fact (which *other* process holds the port) isn't
  something this process can look up about itself, so the fix points at
  how to find it rather than guessing at a PID.

## [1.0.0] - 2026-07-08

### Added
- Validation issues now report the specific affected unit ids and a
  human-readable detail string, not just a count.
- `POST /correct` — applies a single corrected value to a flagged unit
  (e.g. Width) as a session-level overlay and immediately re-validates,
  without needing a full re-upload.
- `POST /exempt-dimensions` — marks a catalog entry that legitimately
  isn't a dimensioned unit (an office, an owner's apartment, etc.) as
  exempt from the "Invalid dimensions" check, instead of requiring a
  fabricated Width/Length.
- `POST /export` accepts `acknowledge_errors` — an explicit human
  override to export despite unresolved validation errors, logged when
  used. Never applied silently.
- Real parsing support for Excel 2003 SpreadsheetML XML, content-sniffed
  regardless of file extension (some facility export tools mislabel this
  format with a `.xls` extension).
- Every session-scoped endpoint now returns a distinct
  `404 Session not found or expired` instead of silently faking a
  zero-value success response.
- `HOST`/`PORT` env vars for the bind address; defaults to `0.0.0.0`
  instead of `127.0.0.1` so the app is reachable from outside a
  container by default.
- `CORS_ALLOWED_ORIGINS` env var to configure allowed origins beyond the
  local dev defaults.
- `version` field on `GET /health`, read from `CARGO_PKG_VERSION`.
- Endpoint-level test coverage (`src/api/*.rs`) for every new endpoint
  and the session-not-found behavior, alongside the existing domain-level
  unit tests.

### Changed
- "Invalid dimensions or area values" simplified to "Invalid
  dimensions" — Area is no longer validated or offered as a correctable
  field.
- Default logging verbosity reduced from per-file `DEBUG` noise to
  aggregate `INFO` summaries per pipeline stage; `RUST_LOG` now actually
  controls the level instead of being force-overridden to `debug`.

### Removed
- The "Area does not match width × length" validation check — Area is a
  derived value (Width × Length), not an independent fact worth
  validating or correcting on its own.

[Unreleased]: https://github.com/quikstorboris/unitprep-api/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/quikstorboris/unitprep-api/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/quikstorboris/unitprep-api/releases/tag/v1.0.0
