# Changelog

All notable changes to `unitprep-api` are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- Calling an endpoint before the session has reached the required
  workflow stage (e.g. `/analyze` before `/validate`) now returns
  `409 Conflict` with a structured `{ error, message }` body, instead of
  a fake all-zero `200` success that looked identical to a real,
  successful "nothing to report" result. Every error response across
  the API now shares this same `{ error, message }` shape.

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

[Unreleased]: https://github.com/quikstorboris/unitPrep/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/quikstorboris/unitPrep/releases/tag/v1.0.0
