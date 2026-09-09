# unitprep-api

<img src="assets/readme/hero.svg" alt="unitprep-api — passkey-authenticated Rust/Axum backend for self-storage onboarding" width="100%" />

UnitPrep is Quikstor's internal platform for self-storage facility
onboarding and QMS migration prep. This is the backend — a Rust/Axum
API serving a client-management surface plus a growing set of
standalone data-prep tools, all behind real passkey authentication. The
frontend is [`unitprep-ui`](../unitprep-ui) (Next.js).

## What it does

**Client management**, sourced from Process Street (Quikstor's
onboarding workflow tool): search and import companies/facilities
directly from Intake, Merchant Account, and Contract Order workflow
runs, then manage each facility's general info, users, Dropbox link,
Elavon merchant account, and policy data (fees, taxes, delinquency,
coverage, specials). A background delta-sync keeps a person-search
index current; a manual re-sync flow surfaces and resolves conflicts
when Process Street data changes after import.

**Three standalone tools**, each its own upload → process → export
session (file-upload-only today — see [Platform
direction](#platform-direction)):

- **Group Prep** — compares UnitGroup names in a facility's unit
  export against a master reference file, flags net-new groups and
  advisory similarity warnings, exports migration-ready artifacts.
- **Duplicate tenant check** — flags multi-unit tenants with
  inconsistent contact info across units and surfaces typo/name-variant
  candidates for human review.
- **Template Tagger** — tags QMS document templates against a
  maintained tag catalog.

**Dropbox integration** — browse and link a facility's own Dropbox
folder; credentials are admin-configurable and stored encrypted, not
hardcoded.

## Authentication

Real, and enforced everywhere. Passwordless WebAuthn/passkeys, TOTP as
the only fallback, admin-issued invites (no self-signup), role-based
permissions instead of hardcoded role checks. See
[AUTHENTICATION.md](AUTHENTICATION.md) for the full architecture and
[THREAT_MODEL.md](THREAT_MODEL.md) for the security posture, including
what's deliberately still open.

First administrator:

```bash
unitprep bootstrap-admin \
  --email you@example.com --first-name You --last-name Example \
  --company quikstor
```

Requires `BOOTSTRAP_DATABASE_URL` — the owner connection, not the
app's own restricted database role. Refuses to run once any user
already exists. Prints a one-time setup token, redeemed through the
same passkey-enrollment endpoint every later invited user goes
through.

## Running

```bash
cargo run
```

Starts on `http://0.0.0.0:8080`. Use `cargo run --release` for
anything CPU-heavy — the dev profile is meaningfully slower for
Excel parsing. Set `CORS_ALLOWED_ORIGINS` (comma-separated) for real
deployed frontend origins; defaults to the local dev servers.

```bash
cargo test
```

556 tests across the workspace: domain-level unit tests alongside the
logic they cover (heaviest on Group Prep's fingerprint-matching
engine, since every false-positive bug this project has hit came from
two structurally different groups being fuzzy-matched as the same
one), plus endpoint-level tests that call handlers directly against a
session built via `src/api/test_support.rs` — no live server or
fabricated multipart bodies needed.

## Project layout

A Cargo workspace, not a single crate:

- `src/` — the `unitprep` binary: HTTP/session orchestration only.
  `src/api/` holds one handler module per endpoint group (auth,
  clients, dedup, tagger, Group Prep's upload/discover/validate/
  analyze/export, Dropbox, Process Street settings).
- `core/` — `unitprep-core`: the tool-agnostic engine (parsing,
  session storage) every tool depends on.
- `unit-group/`, `dedup/`, `template-tagger/` — each tool's own domain
  logic, depending only on `core`. No session state, HTTP, or export
  format — those stay the binary's job.
- `docx-surgeon/`, `tagger-pipeline/` — a DOCX-editing library and the
  glue between it and Template Tagger.

## Platform direction

More tools are expected to arrive on this same backend and session
model over time — this is deliberately a platform, not a fixed set of
three tools. QMS API integration (reading facility/tenant data
directly instead of a manual export-then-upload step) is planned for
tools whose source data actually lives in QMS; duplicate tenant check,
which migrates tenants *from* the legacy QSX system, would stay
file-only regardless.

---

See [CHANGELOG.md](CHANGELOG.md) for what shipped recently and
[AUDIT_RETENTION.md](AUDIT_RETENTION.md) for how audit/activity logs
are retained.
