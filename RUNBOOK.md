# Operations runbook

One page: what has to be configured before this runs, what "a session"
actually means (there are several, with different lifetimes), and what
happens — and what to do — when the process restarts or crashes.

## Required configuration

| Variable | Required? | Notes |
|---|---|---|
| `DATABASE_URL` | Yes | Postgres connection string (Neon in practice). `connect_lazy` — the process starts even if this is briefly unreachable, but nothing that touches the DB will work. |
| `WEBAUTHN_RP_ID` | No (defaults `localhost`) | Must be a valid domain suffix of `WEBAUTHN_RP_ORIGIN`. |
| `WEBAUTHN_RP_ORIGIN` | No (defaults `http://localhost:3000`) | A non-`localhost` origin without `https://` is a **hard startup failure** (`auth::validate_cookie_security`) — session cookies would otherwise travel unencrypted. |
| `SESSION_COOKIE_SECURE` | No | Legitimate escape hatch for local HTTP dev only. |
| `DB_POOL_MAX_CONNECTIONS` | No (default `20`) | Size of the app's Postgres connection pool, 1-200; anything else falls back to 20. See "Database pool and load" below before changing it. |
| `HOST` / `PORT` | No (default `0.0.0.0:8080`) | |
| `CORS_ALLOWED_ORIGINS` | No (defaults to the two frontend dev origins) | Comma-separated. Set to the real deployed frontend origin(s) in any non-dev deployment. |

**Dropbox and Process Street are DB-first, env-fallback** — the admin
Integrations settings page (`integrations.dropbox_configuration` /
`integrations.process_street_settings`) is the real source of truth once configured
there; `DROPBOX_APP_KEY`/`DROPBOX_APP_SECRET`/`DROPBOX_REFRESH_TOKEN`/
`DROPBOX_ROOT_NAMESPACE_ID`/`DROPBOX_ROOT_PATH` and
`PROCESS_STREET_API_KEY` are only the fallback for a deployment that
hasn't configured it there yet. Missing Dropbox config is a **hard
startup failure**; missing Process Street config is not (those
endpoints just return a clear error until configured).

**A saved change on either Integrations settings page takes effect on
the next server restart** — nothing re-reads it mid-run.

## Session lifetimes — there are four different ones

"Session" means several distinct things in this app, each with its own
lifetime and its own store. Don't assume a number that applies to one
applies to another.

| What | Lifetime | Configurable via | Durable across a restart? |
|---|---|---|---|
| A signed-in user's auth session (idle) | 30 min default | `SESSION_IDLE_TIMEOUT_MINUTES` | Yes — DB-backed (`auth.sessions`), always has been |
| A signed-in user's auth session (absolute) | 12h default | `SESSION_LIFETIME_HOURS` | Yes |
| A WebAuthn passkey ceremony (register/login) | 5 min, fixed | not configurable | **Yes, as of `v1.9.39`** — `DurableSessionStore`, `auth.durable_sessions` |
| A tool session (Group Prep / Dedup / Template Tagger — one upload→process→export run) | 10 min default | `SESSION_TIMEOUT_SECS` | **Yes, as of `v1.9.39`** — same mechanism |

## What happens if the process restarts or crashes

**As of `v1.9.39`, this is much less disruptive than it used to be.**
All four session types above write through to Postgres (see
`core/src/durable_session_store.rs`) — a restart mid-upload, mid-report,
or mid-passkey-enrollment no longer strands the user. On the next
request for that session id, the new process cold-hydrates it from
`auth.durable_sessions` and continues exactly as if it had never left
memory.

**What's NOT covered, and still needs a retry**:
- A session that had already idle-timed-out before the restart — gone
  either way, same as before.
- The narrow write-through race: if the process crashes in the
  literal gap between a `save()` returning and its fire-and-forget
  Postgres write completing (milliseconds), that one save is lost.
  Only matters in practice for a crash during a tool session's
  `/upload` (the only `save()` most tool sessions ever make before
  their next mutation supersedes it) — retry the upload.
- Everything else in the app that ISN'T a "session" in the sense
  above (an in-flight HTTP request, a background sync tick) is lost
  on restart the ordinary way any stateless-request server is; there's
  nothing session-shaped to recover there in the first place.

**User-facing symptom either way**: every page that depends on an
existing session already treats an unexpected 404 as "session
expired" and shows an explicit screen rather than a confusing empty
result — so even in the narrow cases above, the failure mode is a
clear message, not silent data loss.

## Deployment model

**Single instance.** Nothing here assumes or supports horizontal
scaling today — `DurableSessionStore`'s in-memory layer is
per-process, so two instances behind a load balancer without sticky
sessions would each keep their own cache and could observe a session
inconsistently between them (the session itself is durable in Postgres,
which stays the source of truth; only the cache is per-process). This is a
deliberate, current limitation,
not an oversight — see `brain/Gotchas.md` / `brain/Patterns.md` in the
vault for the fuller reasoning if this ever needs to change.

## Recovering from a stuck deploy

1. Confirm `DATABASE_URL` is reachable (`psql "$DATABASE_URL" -c 'select 1'`).
2. Check `RUST_LOG=unitprep=debug` output for a startup panic — Dropbox
   misconfiguration and an invalid `WEBAUTHN_RP_ORIGIN`/`SESSION_COOKIE_SECURE`
   combination are the two hard-failure cases above.
3. If sessions look wrong after a restart, check `auth.durable_sessions`
   directly — `SELECT kind, id, last_accessed FROM auth.durable_sessions;`
   — to see what actually persisted.

## Database migrations: the production scripts

Two scripts under `scripts/` operate on the **production** Neon branch. Both
are run **by hand, interactively, by Boris -- never from CI** -- and both read
`NEON_PROD_DATABASE_URL_DIRECT` out of `.env.local` by extracting that single
line (never `source .env.local`: a stray line in it has held a cleartext
password before).

| Script | What it does | Changes anything? |
|---|---|---|
| `scripts/prod_db_status.sh` | Shows the latest applied migration on the prod branch and which local migrations it is missing, plus any migration that failed. | No (read-only) |
| `scripts/prod_db_sync.sh` | Counts pending migrations, then (only after you type `apply prod`) runs `sqlx migrate run` against prod, re-applies the `app_service` grants (`scripts/setup_app_service_role.sql`), and verifies. | **Yes** -- each migration runs in its own transaction, so a failure stops with the earlier ones kept |

Order of operations for a release that includes a migration: run
`prod_db_status.sh` to see what is pending, review the pending migrations'
`.up.sql` files, run `prod_db_sync.sh`, then deploy the code. A chunk whose
Rust code *requires* a new column or table (rather than merely benefiting
from one) must say so in its CHANGELOG entry, because deploying that code
before its migration would break it.

Always apply migrations with sqlx (these scripts, or `scripts/bootstrap_test_db.sh`
for the local test database), never by pasting SQL into `psql`: a hand-run
migration leaves no `_sqlx_migrations` record and the next sqlx run then fails
with "already exists".

`SCHEMA.sql` is a read-only snapshot of the schema the migrations produce;
regenerate it after a migration lands with `scripts/regenerate_schema_sql.sh`.

## Database pool and load (measured 2026-10-09, efficiency refactor)

The pool is **20 connections** (`DB_POOL_MAX_CONNECTIONS`), a caller waits at most 10 s for one (`POOL_ACQUIRE_TIMEOUT`), and connections idle 30 s or longer get a real `SELECT 1` before reuse. CPU-heavy handlers (parsing, matching, XLSX/ZIP/PDF building, sealing) run on tokio's blocking pool, not the async workers.

Measured with `concurrent_load_report` (`src/api/concurrent_load_tests.rs`, ignored; command and knobs in its module doc): the real router, real session cookies and RLS, a local Postgres, 4 server worker threads, 2,400-row dedup uploads with no think time, plus a cheap canary request (`GET /health/whoami`) every 20 ms. The canary's latency is what other users feel while operators run heavy tools. Machine: 22 cores, loopback Postgres 18 in Docker, so read the ratios, not the milliseconds.

| Concurrent dedup operators | Canary p50 / p95 / p99 (ms) | Dedup check p50 / p95 (ms) | Checks per second | Peak pool in use |
|---|---|---|---|---|
| 0 (idle) | 1.3 / 1.7 / 2.1 | - | - | - |
| 1 | 2.3 / 3.5 / 5.4 | 200 / 228 | 4.9 | 6 of 20 |
| 4 | 2.5 / 5.5 / 9.4 | 234 / 267 | 17.2 | 5 |
| 8 | 3.1 / 7.3 / 14 | 294 / 376 | 26.8 | 11 |
| 16 | 3.9 / 15 / 25 | 375 / 750 | 38.7 | 18 |

No request failed at any level. With only 2 server workers, 16 operators gave a canary of 5.3 / 49 / 88 ms and filled the pool (20 of 20), still responsive. Running the same work inline on the async workers (the pre-refactor shape) made the canary 39 ms at 4 operators and 857 ms at 16, so keeping CPU off the workers is what holds this up.

**Sizing for a remote database.** Through a 20 ms round-trip stand-in for a remote Neon, a single dedup check costs about 690 ms instead of 200, and the pool reaches 20 of 20 at 8 operators: **with a remote database the pool, not the CPU, saturates first.** Rules of thumb, in order:

1. Watch for `PoolTimedOut` (a request failing after 10 s waiting for a connection). If it appears under normal use, raise `DB_POOL_MAX_CONNECTIONS` in steps of 10.
2. Never exceed what the database allows across ALL instances: on Neon, stay under the pooled endpoint's client limit and remember a second app instance doubles the total. Reload the value only by restarting.
3. After any change, rerun `concurrent_load_report` through `dev-tools/latency_proxy.py --delay-ms 10` and compare the peak pool figure and the canary tail.

## Stored tool runs (retention)

`client_ops.tool_runs` keeps every Onboarding Work run per facility, indefinitely. A dedup run of 2,400 rows stores about **1.9 MB** (encrypted source file about 272 KB plus encrypted parsed records about 1.6 MB; encrypted data does not compress), so **1,000 runs is about 1.9 GB** on Neon. No retention policy exists yet, deliberately: dropping a run's stored records or source removes a facility's history and the ability to re-run matching from it, which is a data-deletion decision for the business, not an efficiency tweak.

Run `scripts/tool_runs_storage.sql` (read-only) for the current footprint by tool and by age. Decide before either of these: stored runs pass roughly 500, or the move to a cloud host. The options, cheapest first: (a) keep everything and pay for storage; (b) after N days keep only the report summary and output, and drop `records` and the source file (a run can then no longer be re-matched, only viewed); (c) archive old rows to cold storage. Until then nothing deletes anything.
