# Threat Model & Control Matrix

Phase II hardening item 6. A structured statement of what unitprep-api's
authentication and authorization system defends against, what specifically
defends against it, and — just as importantly — what it does not yet
defend against and why that's an accepted, named gap rather than an
oversight. [AUTHENTICATION.md](AUTHENTICATION.md) explains the *design*;
this document inventories the *threats* and cross-checks that every one
has an owner.

Scope: the auth/session/audit system (`src/auth`, `src/api/auth_*.rs`,
the `auth` Postgres schema) and the product routes it gates. Out of
scope: application-level input handling unrelated to identity (CSV
injection, XLSX export safety, etc. — covered separately in code review),
and infrastructure the deployment hasn't chosen yet (hosting platform,
CDN/WAF).

## Assets

What this system protects, roughly in order of blast radius if
compromised:

1. **Passkey credentials** (`auth.webauthn_credentials`) — the primary
   auth factor. Public key material only; the private key never leaves
   the authenticator.
2. **Session tokens** (`auth.sessions`) — bearer access to an
   authenticated identity for the token's lifetime.
3. **TOTP secrets** (`auth.totp_credentials`) — the one credential this
   system holds a reproducible copy of at all (see AUTHENTICATION.md's
   comparison table).
4. **The audit trail** (`auth.auth_audit_logs`) — the record an operator
   or a future pentest/auditor relies on; its own integrity is an asset.
5. **Admin capability** — the ability to create invites, revoke sessions,
   deactivate accounts, and (via account recovery) revoke and reissue
   every credential on an account.
6. **Product data behind the tool routes** (dedup, unit-group) — gated by
   authentication but not covered further here; see their own domain
   docs.

## Actors and trust boundaries

| Actor | Can reach | Trusted to |
|---|---|---|
| Anonymous internet caller | `/auth/register/begin\|finish` (invite path), `/auth/login/begin\|finish`, `/auth/invites/recover` isn't reachable unauth — admin only | Nothing. Every response on these paths is deliberately opaque (see "User enumeration" below). |
| Invited-not-yet-registered user | The invite-redemption registration path, with an unguessable token | Possession of the token, nothing else. |
| Authenticated non-admin | Every tool route, TOTP self-service (enroll/disable/step-up), sign-out | Their own identity only — every RLS policy in the schema that isn't `_admin_only` is `owner_only` or `own_or_admin`. |
| Authenticated admin | Everything a non-admin can, plus `/auth/invites`, `/auth/invites/recover`, `/auth/users` | Their own identity, plus the `current_setting('app.current_user_role') = 'admin'` bypass wired into each RLS policy that grants it. |
| `app_service` (the DB role the API connects as) | Whatever RLS + column grants allow | Nothing by default — RLS is deny-by-default on every table in the schema, and even where a table's RLS would allow a write, column-level grants narrow it further (see the users-table row below). |
| The Postgres owner role / migration role | Everything, RLS included | Full trust — this is the standard Postgres "whoever runs migrations owns the schema" assumption, not something this system tries to defend against. Used deliberately, once, by `bootstrap-admin` (see below). |

## Trust assumptions this model depends on

These are load-bearing and should be re-checked if the deployment changes:

- **Direct exposure, no reverse proxy in front of unitprep-api today.**
  The raw TCP peer address (`ConnectInfo`) is trusted as the real client
  IP for rate limiting and the anomaly signal. If a reverse proxy or CDN
  (Cloudflare, most likely, per current planning) is ever put in front,
  this assumption breaks silently unless a trusted-forwarded-header
  policy is designed at the same time — see the "Not yet built" section
  below and [[Two Phase II follow-on features logged, trigger-gated for
  later]] in the vault.
- **The database connection is genuinely `app_service`, not an owner
  connection.** `/health/db` exists specifically to catch a
  misconfigured `DATABASE_URL` pointing at the owner role, which would
  silently bypass every RLS policy in this matrix while looking correct
  from the application's point of view.
- **`TOTP_ENCRYPTION_KEY` is only as protected as the process
  environment / `.env.local`.** This is a named, accepted stopgap — see
  the KMS row below.
- **Session cookies are the only bearer credential in play.** There is no
  API-token/service-account auth model to reason about separately.

## Threat / control matrix

| Threat | Primary control(s) | Where | Status / residual risk |
|---|---|---|---|
| Password-based credential theft (phishing, credential stuffing, reuse) | No passwords exist anywhere in the system. WebAuthn is origin-bound and phishing-resistant by construction. | `src/auth/webauthn_backend.rs`, all `auth_register.rs`/`auth_login.rs` | Structurally closed, not mitigated — there is no password to steal. |
| TOTP code phishing / real-time relay | TOTP is step-up-only for an already-authenticated session, never a login factor — see `auth_totp.rs`'s module docs. A relayed code can extend a session that's already gated, not create one. | `src/api/auth_totp.rs` | Accepted residual risk, explicitly documented: TOTP is "neither phishing-resistant nor MFA-strength on its own," which is why it's never allowed to be a full login path. |
| TOTP code brute force | 6-digit code, 3-step (90s) acceptance window; 5 failed attempts locks the credential for 15 minutes. Lockout is safe only because TOTP is never the sole path to an account (passkey login doesn't consult it). | `auth.record_totp_failure`, `auth.totp_credentials.locked_until` | If TOTP is ever promoted to a mandatory/primary factor, this lockout becomes an account-denial primitive — flagged explicitly in code comments as a re-evaluation trigger. |
| TOTP code replay (same code submitted twice inside its window) | `auth.totp_credentials.last_used_step` — a submitted code matching an already-accepted step or earlier is refused, even inside the ordinary skew window. | `auth::totp::verify_code`, migration `20260804130000` | Closed 2026-08-04 (Phase II item 2). |
| Account left with no working step-up factor mid-re-enrollment (abandoned tab, dead battery, anything) | `auth.totp_credentials.pending_secret_encrypted` holds the re-enrollment candidate separately — the existing confirmed `secret_encrypted` is untouched until a code verifies against the *pending* one, at which point it's promoted. Previously the candidate overwrote the live secret immediately at `/enroll/begin`. | `src/api/auth_totp.rs`, migration `20260804150000` | Closed 2026-08-04. There is also no "disable TOTP" action any more (same migration) — no security upside to a zero-step-up-factor state, only a self-lockout risk. |
| An admin misconfiguring or accidentally emptying `step_up_actions` disables step-up protection for a real action | None at the database layer beyond RLS restricting writes to admins — an admin who edits `auth.auth_configuration.step_up_actions` down to `[]` genuinely removes the `add_passkey` gate, by design (that's the point of making it configurable). | `auth::step_up_policy`, migration `20260804150000` | Accepted — this is a deliberate tradeoff (admin-tunable policy over a hardcoded check), not an oversight. No UI exists yet to edit this row at all, so it's a new risk only in the sense that direct SQL access could do it; revisit if/when an Admin > Security UI is built for it, since a UI presumably wants its own confirmation step for turning off a step-up gate. |
| Session token theft (XSS, log leakage, DB dump) | `httpOnly` (unreadable to page JS); DB stores only a SHA-256 hash, never the raw token, so a DB read alone yields nothing usable. | `src/auth/session_cookie.rs`, `auth.sessions.token_hash` | An active XSS bug on the *same page* could still ride along on live requests — no defense in this system beats that; standard web XSS hardening (CSP, output encoding) is out of this document's scope. |
| Session token theft via cross-site request | `SameSite=Strict` on session and ceremony cookies — never sent on a cross-site request, including a top-level navigation from another site. | `src/auth/session_cookie.rs`, `src/auth/ceremony_cookie.rs` | Closed 2026-08-04 (Phase II item 2). Cost: a signed-in user opening a link to the app from another site won't carry the cookie on that first hop — accepted tradeoff for an internal tool. |
| Session fixation / cookie tampering | Token is 256 bits of CSPRNG output, generated server-side only, never accepted from a client-supplied value. | `src/auth/session_token.rs` | Closed by construction — there is no code path that accepts a client-chosen token. |
| Stolen-but-live session used indefinitely | Absolute expiry (`SESSION_LIFETIME_HOURS`, 12h default) plus idle expiry (`SESSION_IDLE_TIMEOUT_MINUTES`, 30min default) — either expiring makes `resolve_session` return nothing. | `auth.resolve_session`, migration `20260804130000` | Closed 2026-08-04 (Phase II item 2). |
| Session un-revocation (an attacker or bug reviving a signed-out session) | `app_service` holds **no** `UPDATE` grant on `auth.sessions` at all, not even column-scoped — revocation can only move forward through `auth.revoke_session`/`auth.revoke_all_sessions_for_token`, both `SET revoked_at = now()` with no caller-supplied value reaching the column. | Migration `20260730140000` | Closed. |
| Login/registration/invite endpoint abuse (credential stuffing, scripted brute force) | Peer-IP-keyed rate limiting, one shared bucket across all anonymous auth endpoints so spreading attempts across endpoints doesn't multiply budget; a separate, more generous bucket for authenticated admin invite creation. | `src/api/mod.rs`'s `GovernorLayer` wiring | Coarse behind a reverse proxy that doesn't preserve the real peer address — not applicable today (direct exposure), revisit if that changes (see Trust Assumptions). |
| User enumeration via login/registration/TOTP responses | Every rejection reason on an anonymous path collapses to one indistinguishable response (`login_unavailable`, `registration_unavailable`) — unknown email, wrong password-equivalent, inactive account, and "no passkey enrolled" are not separable by the caller. | `src/api/auth_login.rs`, `auth_register.rs` | Closed — verified by dedicated tests asserting identical response shapes across every rejection reason. |
| Privilege escalation via self-service profile update | Column-level `UPDATE` grant on `auth.users` restricted to `(first_name, last_name, job_title)` — `role`/`status`/`email` are not grantable columns at all, so a bug that accepts a `role` field from a client request still can't write it. | Migration `20260729210000` | Closed. Anything touching `role`/`status` must go through an admin-checked `SECURITY DEFINER` function. |
| An admin abusing legitimate admin capability (insider risk) | Every administrative act (invite creation, account recovery, user status change) is audited with actor and target recorded separately, never conflated. | `auth_audit_log.rs`'s `Subjects` type, `INVITE_CREATED`/`ACCOUNT_RECOVERY_INITIATED` events | Detective, not preventive — this system has one privileged role (`admin`) with no further separation of duties. Accepted for the current scale (small, trusted admin population); revisit if the admin population grows past "everyone knows everyone." |
| Anomalous / attacker-controlled login on a stolen-but-valid credential set | A login from an IP or `user_agent` never seen before for an account with prior history is audited unconditionally and gated behind an immediate TOTP step-up when the account has TOTP confirmed. | `src/api/auth_login.rs`'s `assess_login_risk`, migration `20260804140000` | Closed 2026-08-04 (Phase II item 4) for accounts with TOTP enrolled. An account with **no** TOTP confirmed is audited but not gated — see the matching row below. |
| Anomalous login on an account with no step-up factor enrolled | Audited (`login_anomaly_detected` with `step_up_required: false` in the metadata) so an operator can see it. | Same as above | **Accepted, named residual risk** — there is no factor to gate with, and forcing a lockout over a self-service factor nobody set up would be a denial-of-service on that account, not a hardening measure. Mitigation is encouraging/requiring TOTP enrolment broadly, not a technical control on this path. |
| Location-based anomaly detection | Scoped down to "new IP address," not true geolocation. | Same as above | **Deliberately deferred** — true geo needs a GeoIP database as a new dependency. Revisit alongside Cloudflare adoption, which would provide country-level geolocation via `CF-IPCountry` for free — see the vault's trigger-gated backlog. |
| Signature/counter cloning of a passkey (cloned authenticator) | webauthn-rs's signature-counter check; the updated counter is persisted on every successful assertion, so a cloned authenticator with a stale counter is detected on its next use. | `auth_login.rs`'s `persist_credential_use` | Standard WebAuthn anti-cloning property; relies on the authenticator actually implementing a monotonic counter (not all do, e.g. some platform authenticators report 0 always — a known WebAuthn-ecosystem limitation, not specific to this codebase). |
| Synced (non-hardware-bound) passkey compromise via password-manager account takeover | None currently — synced passkeys are explicitly accepted (see AUTHENTICATION.md). `device_bound` is recorded at enrolment for visibility. | `auth_register.rs` | **Deliberately deferred** (Phase II item 1, hardware-bound passkey policy) — team hasn't yet decided whether to require hardware keys, and for whom. |
| Lost-credential lockout / account recovery abuse | Admin-mediated only: the locked-out user contacts the admin out-of-band (Teams/phone, serving as the identity-reconfirmation step), the admin revokes every existing credential and reissues an invite. No email/ESP dependency, no self-service recovery flow to abuse. | `auth_invites.rs`'s `recover_account`, `auth.set_user_status` | Single point of failure is the admin population itself being reachable — see "First-administrator break-glass," next row. |
| Total lockout (the only admin loses their own device) | `unitprep bootstrap-admin` CLI subcommand, connecting as the Postgres owner (not `app_service`), with no HTTP surface at all — the guard against minting an admin is structural, not configuration-gated. | `src/bootstrap.rs` | The CLI "still works" is confirmed; whether break-glass access (owner DB credentials, hosting/Neon account access, `TOTP_ENCRYPTION_KEY`) doesn't quietly depend on one specific reachable person is a standing open item from the Phase I roadmap, not yet closed. |
| SQL injection | Every query in the codebase is parameterized via `sqlx`'s bind parameters; no string-interpolated SQL exists in the auth surface. | Throughout `src/api/auth_*.rs`, `src/auth/*.rs` | Structurally closed by the query-construction convention used everywhere, not something enforced by a separate control. |
| Cross-schema / search_path confusion attacks | Every `SECURITY DEFINER` function pins `SET search_path = auth, public`; post-move migrations schema-qualify table references directly rather than relying on the pin alone. | Migration `20260723150000` and everything after | Closed; see the vault's own gotcha notes on why this needed explicit attention after the schema move. |
| Audit trail gap (an action happens with no corresponding record) | Closed for the asymmetry that was found and fixed: a rejected registration now writes `registration_failed` the same way a failed login always wrote `login_failed`. Every login, TOTP, invite, and recovery outcome (success and failure) has a matching event constant in `audit_log::event`. | `src/auth/audit_log.rs` | No automated coverage check exists to catch a *future* handler that forgets to audit — Phase I item 1's closing verification was a manual read-through, not a linter or test. Revisit if the handler count grows large enough that a manual pass stops being reliable. |
| Audit trail tampering (rows altered or deleted after the fact) | `INSERT`-only RLS policy (`auth_audit_logs_insert_always`); no `UPDATE`/`DELETE` policy exists at all, so even an admin-context connection cannot modify or remove a row through the application role. | Migration `20260721204636` | Closed against `app_service`. Not closed against the Postgres owner role, which can do anything to any table — accepted, standard assumption (see Trust Assumptions above). |
| Multi-instance / horizontal-scaling inconsistency in in-memory state | None — `AppState` holds four separate `InMemorySessionStore` instances (WebAuthn's `RegistrationCeremony`/`AuthenticationCeremony`, plus the unit-group and dedup tool-session stores), all process-local. | `unitprep_core::in_memory_session_store` | **Scoped and deferred 2026-08-04** — a real fix isn't ceremony-specific: all four stores share the identical limitation, so a correct fix means a shared backend (Redis or Postgres-backed) everywhere at once, not a small targeted change. Also arguably a downgrade for single-instance security in the meantime (WebAuthn challenge state currently never leaves the process). No multi-instance deployment is planned; revisit only if horizontal scaling is actually needed. |
| Formal adversarial validation (as opposed to design review) | None yet — everything in this matrix is the product of code review and reasoning about the system, not someone actually trying to break it. | — | **Deliberately deferred**, very low priority currently, not confirmed this project will ever need one. Named here anyway because a threat model that omits "nobody has actually attacked this" would be dishonest about its own confidence level. |

## Known gaps, named rather than hidden

Everything below is a genuine, current gap — listed here so this document
stays honest rather than only cataloguing what's already closed:

- Hardware-bound passkey policy (Phase II item 1) — deferred, scope
  undecided.
- Real KMS for `TOTP_ENCRYPTION_KEY` (Phase II item 3) — deferred, not
  taking on a cloud-provider dependency at this point.
- Formal external penetration test (Phase II item 5) — deferred, low
  priority.
- True geolocation for the anomaly signal — deferred, pending either a
  GeoIP database decision or Cloudflare adoption.
- In-memory session/ceremony state horizontal-scaling fix — deferred
  2026-08-04, only matters at multi-instance scale; scope turned out to
  span all four `InMemorySessionStore` instances, not just WebAuthn
  ceremonies (see the matrix row above).
- No "remember this device" mechanism — every anomalous login requires a
  fresh step-up, with no way to mark a device as previously trusted.
  Logged as a trigger-gated future feature (see the vault).
- `unitprep-ui` has no step-up UI yet — an anomalous login on a
  TOTP-enrolled account currently 403s on every route except `whoami`
  with no frontend explanation of why.
- No standalone "deactivate a user" admin action — `auth.set_user_status`
  (the underlying primitive, admin-gated) exists and is exercised by the
  account-recovery flow, but nothing exposes it as its own endpoint, so
  there's no way to deactivate an account outside of full recovery.
  Frontend has no button for it either, for the same reason. Scheduled
  as a small follow-up after this Phase II close-out.
- Audit rows don't yet carry `ip_address`, `before_state`, or
  `after_state` — all three columns exist in `auth.auth_audit_logs`
  (since the original schema) but `audit_log::record()` has never
  written any of them. Relevant if/when a frontend audit-log viewer with
  before/after diffing is built — see the vault for that discussion.
- `auth.auth_configuration.step_up_actions` and `allowed_factors` have no
  admin UI yet — the only way to edit either today is direct SQL. Fine
  for now (one admin, low change frequency); revisit once an
  Admin > Security policy tab is built, since a UI should presumably
  confirm before someone turns off a step-up gate.
- Rate-limit rejections (`429` from the auth/invite `GovernorLayer`) and
  a session presented after it's expired aren't audited or even
  `tracing`-logged yet — both flagged as gaps worth closing, not yet
  built.

## Review cadence

See [AUDIT_RETENTION.md](AUDIT_RETENTION.md) for how long the audit trail
this matrix depends on is actually kept, and who is expected to review
it. This document itself should be revisited whenever a row in the
matrix changes state (a deferred item gets built, a new threat surface
is added — e.g. the frontend shipping, a reverse proxy being adopted) —
there is no calendar-based review cycle for the threat model itself yet;
consider adding one if this system's risk profile or user base grows
materially.
