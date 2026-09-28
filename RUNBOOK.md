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
inconsistently between them. This is a deliberate, current limitation,
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
