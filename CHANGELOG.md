# Changelog

All notable changes to `unitprep-api` are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.8.0] - 2026-08-11

Continued growing the QMS tag catalog from real-world evidence, and
shipped the write-side counterpart to the Phase 2 matching engine.

### Added
- **`e.dob`, `e.add1`, `e.add2`** — date of birth and the two-line
  address variant, confirmed directly against the live QMS tag
  picker. `e.address` (single-line) was already seeded.
- **`m.indate`, `m.secdep`, `l.indate`, `l.secdep`, `d.now`,
  `d.nowlong`** — identified from a real sample lease (Affordable
  Storage) for the QMS Template Tagging Assistant effort; each key
  independently confirmed in the vault's own transcribed tag-family
  notes before being added.
- **98 more tags**, harvested by scanning every `{{tag}}` occurrence
  across 258 already-tagged real client documents (the full QMS
  Onboarding document tree). Introduces six categories beyond the
  original Tenant/Unit/Lease/Move-In/Date-Time set: Alternate Contact,
  Military, Facility, Company, Vehicle, Lienholder, Signature. The
  catalog is now 121 tags, up from 13.
- **`docx-surgeon`**, a new standalone crate: surgical, minimal-diff
  text editing inside a `.docx` file. Given a document and a set of
  exact text-span edits, produces a new document where only those
  spans change — every other run, table, style, and zip part is
  copied through unchanged. Never deserializes the whole document
  into an object model; edits are spliced directly into the original
  XML bytes at each targeted run's own text range, with a decode/
  re-encode round trip that makes it impossible to corrupt a run
  whose text contains an XML entity. Proven against a real sample
  document, not just synthetic fixtures: every zip entry other than
  `word/document.xml` verified byte-identical after an edit, and
  everything in `document.xml` outside the one targeted run's text
  verified byte-identical too. Not yet wired into the template-
  tagging pipeline or any HTTP endpoint.

## [1.7.0] - 2026-08-10

Phase 1 of the QMS Template Tagging Assistant shipped: `client_ops`, the
first Postgres schema outside `auth`, holding a hand-maintained reference
catalog of QMS's document-template merge tags — a stand-in until QMS
exposes its own tag list via its own API. Seeded with the 13 tags QMS's
own Default Lease document calls out as its "popular variables," not the
full ~300+ tag catalog — growing this, and adding real context-scoping
(a tag can be valid in some document contexts and not others), is
tracked follow-up work, not a rework of what shipped here.

### Added
- **`client_ops.qms_tag`**: `tag_key` (natural key, matched against
  literal `{{tag_key}}` text in a document — never renamed), `label`,
  `category`, `is_active`. Never hard-deleted: deactivate/reactivate
  only, so a template already referencing a tag stays resolvable, or at
  least visible as deactivated, rather than disappearing outright.
- **`GET/POST /client-ops/qms-tags`, `PUT /client-ops/qms-tags/{tag_key}`,
  `PATCH /client-ops/qms-tags/{tag_key}/deactivate`,
  `PATCH /client-ops/qms-tags/{tag_key}/reactivate`.** Read is open to
  any authenticated caller (catalog/reference data, nothing sensitive);
  every mutation requires the new `client_ops.manage_tags` permission
  and writes a `client_ops.audit_log` row.
- **`client_ops.manage_tags` permission**, granted to `admin`,
  `onboarding_manager`, and `department_manager` alike — deliberately
  not the same shape as `client_ops.perform`, which `admin` does not
  hold. Maintaining a reference catalog of tag names reads as system
  configuration, not a client operation, so `admin` shares this one
  without blurring that boundary.
- **`client_ops.audit_log`**: a distinct, non-security operations trail
  for client-ops mutations (today: `qms_tag` edits; later: client
  credential adds/revokes and whatever else this domain grows). Kept
  separate from `auth.auth_audit_logs` on purpose — the same
  access-boundary split already locked between Admin's oversight audit
  and client-ops's own business data.

## [1.6.0] - 2026-08-07

Phase II (hardening) items 2, 4, 6, and 7 shipped: session/TOTP
hardening, anomaly/risk-based login signals, a formal threat model, and
audit retention/review documentation. Item 8 (ceremony-state
horizontal-scaling fix) scoped and deferred the same day — see
THREAT_MODEL.md. Phase II is closed out; nothing left on it is
scheduled, only trigger-gated.

### Added
- **Idle session expiry.** `auth.resolve_session` now takes a
  `p_idle_minutes` argument and refuses a session whose `last_seen_at`
  is older than that window (`SESSION_IDLE_TIMEOUT_MINUTES`, default
  30), independent of the existing absolute expiry
  (`SESSION_LIFETIME_HOURS`, default 12h, unchanged).
- **TOTP replay window.** `auth.totp_credentials` gains
  `last_used_step`, the TOTP time-step last accepted for that
  credential. `auth::totp::verify_code` now matches a submitted code
  against a specific candidate step (rather than trusting an opaque
  yes/no) and refuses one matching that step or an earlier one, closing
  the window where an observed code stayed replayable for the rest of
  its ~90s skew window.
- **Anomaly/risk-based login signal.** A login from an IP address or
  `user_agent` never seen before for an account with prior session
  history is now flagged: recorded as a new `login_anomaly_detected`
  audit event unconditionally, and gated behind an immediate TOTP
  step-up (`auth.sessions.requires_step_up`) when the account has TOTP
  confirmed. `AuthenticatedUser` refuses every route except
  `/auth/totp/step-up` and `/health/whoami` while the flag is set;
  `auth.record_step_up` clears it on a successful step-up.
  `auth.sessions.ip_address` is now actually populated (via
  `ConnectInfo`, direct-exposure topology) instead of always `NULL`.
  `/auth/login/finish` and `/health/whoami` responses both gained a
  `step_up_required` field.
- **Admin-configurable step-up policy.** `auth.auth_configuration.step_up_actions`
  is now actually read (via `auth::step_up_policy`) instead of sitting
  unused — gates "add a passkey to an account that already has one",
  previously hardcoded as unconditional. Seeded with `["add_passkey"]`
  so wiring this up doesn't silently disable existing protection. A new
  RLS policy lets any authenticated caller (not just admins) read
  `auth_configuration`, since an ordinary user needs to check whether
  their own action is gated.
- **TOTP re-enrollment no longer has a no-step-up-factor gap.**
  `auth.totp_credentials.pending_secret_encrypted` holds the
  re-enrollment candidate; the existing confirmed secret stays live
  until a code verifies against the pending one and gets promoted.
  Previously `/enroll/begin` overwrote the live secret immediately, so
  an abandoned re-enrollment left the account with no working step-up
  factor until it was finished.

### Removed
- **`POST /auth/totp/disable`** and the frontend's "Remove authenticator
  app" button. TOTP is step-up-only, never a login factor, so there was
  no security benefit to letting an account have zero step-up factor —
  only a self-inflicted-lockout risk. The account page now offers
  "Update authenticator app" (re-enrollment) instead, which replaces the
  factor rather than removing it with nothing to replace it.
- **`auth.auth_configuration.mandatory_passkey_enrollment`** — no code
  path ever made passkey enrollment optional; the column implied a
  control that didn't exist.

### Changed
- Session and ceremony cookies now carry `SameSite=Strict` (was `Lax`).
- `auth.create_session` gained a `p_requires_step_up` argument (no
  default — every caller now passes it explicitly).

### Documentation
- **[THREAT_MODEL.md](THREAT_MODEL.md)** — a formal threat/control
  matrix for the auth system: every threat considered, the control that
  closes it and where, and every deferred item or known gap named
  explicitly rather than left implicit.
- **[AUDIT_RETENTION.md](AUDIT_RETENTION.md)** — retention policy
  (indefinite by default, and structurally so — the audit table's
  append-only triggers block deletion outright) and a trigger-driven
  review process with runnable queries.

The backlog approved right after Phase II's close-out also shipped, found
via a real user bug report ("disable user feature is not available in the
FE") and the audit-log-viewer questions it raised: a standalone
disable-user action, the two audit-log gaps that were blocking a frontend
viewer, three new audit event types, the `onboarding_manager` role, and a
way to actually assign it.

### Added
- **`POST /auth/users/{id}/deactivate`** — admin-gated, wraps the
  already-built `auth.set_user_status` primitive in its own endpoint
  rather than only being reachable indirectly through account recovery.
  Refuses on self, on an already-deactivated target, and on a concurrent
  status change; writes a `user_deactivated` audit row with a real
  before/after status diff. `unitprep-ui`'s admin Users table gained a
  matching Disable button with a confirm step.
- **`GET /auth/audit-logs`** — admin-only listing over
  `auth.auth_audit_logs`, filterable by `event_type` and `user_id`
  (matches actor or target), keyset-paginated by `id`. No new `SECURITY
  DEFINER` function needed — `auth_audit_logs_select_admin_only` already
  grants exactly this access, unlike `list_users_for_admin`, which exists
  to bypass a *different* table's owner-only RLS for a cross-user join.
  Backs `unitprep-ui`'s new Audit Logs page: filters, keyset "load more",
  and a red/green before/after diff view for the events that carry one.
- **`audit_log::record()` now takes `ip_address` and a `Change`
  (before/after) pair.** Both columns existed in `auth.auth_audit_logs`
  since the very first migration with nothing ever writing them. Every
  existing call site was updated — `ip_address` is populated wherever
  `ConnectInfo` was already in scope or was cheap to add (login/
  registration success paths, invite creation/recovery, the new
  deactivate-user and role-change actions); the `/begin` legs and TOTP
  handlers still pass `None`, since neither has a natural IP source
  without disproportionate churn. `before_state`/`after_state` are
  populated for the schema's named diff-worthy events
  (`user_deactivated`, `account_recovery_initiated`, `role_changed`).
- **Three new audit event types.** `rate_limit_rejected` (fired from the
  auth/invite `GovernorLayer`'s error handler for the caller-driven
  rejection case only — the handler is synchronous with no `ConnectInfo`
  available, so this one carries no `ip_address`); `session_expired_access_attempt`
  (a session that genuinely existed and crossed its idle or absolute
  expiry, backed by a new `auth.check_session_expired` function —
  distinct from an ordinary missing/forged cookie, which still gets a
  plain 401 with no row); `authorization_failure` (an authenticated
  caller reaching an admin-gated action without the role for it).
- **`onboarding_manager`**, a second `auth.auth_role` enum value and
  `Role` variant — the second role named in the original architecture
  doc's extensible-role-column design. Every admin-gated `match
  admin.role` — unreachable while `Role` had one variant — gained an
  explicit arm that refuses it via a new shared `insufficient_role()` 403
  and writes an `authorization_failure` row.
- **Role selection.** `CreateInviteRequest` gained a `role` field
  (validated against `Role::from_db_text`, now public so a request-body
  validator and the session extractor share one parser) — any admin may
  assign either role at invite-creation time, and reissuing re-applies
  whatever role is submitted. `POST /auth/users/{id}/role` changes an
  already-enrolled user's role via a new `auth.set_user_role` `SECURITY
  DEFINER` function (mirroring `set_user_status` — `role` has no direct
  `UPDATE` grant), refuses a caller changing their own role, and writes a
  `role_changed` audit row with a before/after diff. `unitprep-ui` gained
  a role dropdown on the invite form and a per-row role dropdown on the
  admin Users table.

### Documentation
- **THREAT_MODEL.md** — new matrix rows for rate-limit abuse,
  session-expiry re-use, and role-based authorization failure; three
  `Known gaps` entries closed (disable-user, audit `ip_address`/before-
  after, rate-limit/session-expiry auditing); a new gap named
  (`onboarding_manager` has no permissions of its own yet — assigning it
  is solved, what it can do is still open).
- **AUDIT_RETENTION.md** — the trigger-driven immediate-review list
  gained `user_deactivated`, `role_changed`, and `authorization_failure`;
  practical queries extended to use `/auth/audit-logs` as the normal
  operational path, with raw SQL kept as the fallback.

Multi-role authorization: a user can now hold more than one role at
once, and every admin-gated endpoint checks a real permission instead of
matching on a hardcoded role name. `onboarding_manager`'s "no
permissions of its own yet" gap (named above) is closed as part of this.

### Added
- **Roles and permissions are now data**, not a single `auth.users.role`
  column: `auth.roles`, `auth.permissions`, `auth.role_permissions`, and
  `auth.user_roles` (many-to-many). Four system roles seeded (`admin`,
  `onboarding_manager`, `department_manager`, `sales`) with an initial
  8-key permission catalog matching each role's agreed capabilities.
  `auth.users.role` and the `auth_role` enum are dropped once nothing
  references them. RLS: the catalog tables are readable by any
  authenticated caller; `user_roles` is owner-or-admin read, admin-only
  write, with a `WITH CHECK` that structurally refuses a caller granting
  or revoking a role on their own account -- enforced by Postgres
  itself, not just the handler.
- **`AuthenticatedUser::require_permission`** replaces the `match
  admin.role { Role::Admin => {}, Role::OnboardingManager => {
  ...403... } }` block that had been duplicated across every admin-gated
  handler. Checks a permission key, records an `authorization_failure`
  audit row on refusal, and returns the existing shared 403. The closed
  `Role` enum is gone -- roles are open-ended now, so hardcoding them in
  Rust would defeat the point.
- **`POST /auth/users/{id}/roles`** (grant) and **`DELETE
  /auth/users/{id}/roles/{role_key}`** (revoke) replace the single-value
  `POST /auth/users/{id}/role` and the `auth.set_user_role` function it
  used -- plain RLS-scoped INSERT/DELETE on `auth.user_roles`, no
  `SECURITY DEFINER` function needed this time, since the table's own
  policies are the real enforcement. Revoking `admin` re-implements the
  last-remaining-admin guard against a role count instead of a role
  column. Both write `role_granted`/`role_revoked` audit rows carrying
  the target's full before/after role set, not just the one role that
  changed.
- **`GET /auth/roles`** -- the role/permission catalog, for the admin
  Roles page and any future role picker. No permission gate: any
  authenticated caller can already read this under RLS, and there's
  nothing sensitive in a role's name or its permission list.
- **`GET`/`PUT /auth/configuration`** -- org-wide auth policy, gated by
  a new `security_policies.manage` permission. Scoped to
  `step_up_actions` only: `allowed_factors` exists in the schema but no
  code path reads it, so a control for it would edit a value with no
  effect on real behaviour.
- Every user-creation path (invite issuance, the `bootstrap-admin` CLI)
  now grants a role as a second insert into `auth.user_roles` rather
  than a column value on the `INSERT INTO auth.users`.

### Fixed
- **`auth.resolve_session` was missing `permission_keys` entirely** --
  designed and coded against in `AuthenticatedUser`, never actually
  migrated into the database function. Every test exercised only the
  unauthenticated (no-cookie) path, so this shipped invisibly until a
  real login hit it. `resolve_session` now returns `permission_keys
  TEXT[]` alongside `role_keys`, resolved in the same query.

### Changed
- **`district_manager` renamed to `department_manager`** (role key and
  label), same day it was created -- "district manager" is already
  self-storage-industry terminology for a client-side facility manager,
  a different concept from this internal staff role.



## [1.5.0] - 2026-08-04

Phase 1 item 8: the admin Users listing that backs `unitprep-ui`'s new
Users tab.

### Added
- **`GET /auth/users`**, admin-only, read-only. Returns every non-deleted
  user's identity, role, status, and two facts the UI needs to decide
  what action makes sense for that row: `credential_count` (passkeys
  enrolled) and `totp_enrolled`.
- **`auth.list_users_for_admin()`**, a new `SECURITY DEFINER` function
  backing it. A plain admin-scoped query can't do this join itself:
  `auth.webauthn_credentials`'s RLS policy has no admin-bypass clause
  (unlike `auth.users`/`auth.totp_credentials`), so an ordinary admin
  query would see only its own credential rows and silently read every
  other user's `credential_count` as zero. This function checks the
  caller's role explicitly, the same way `auth.set_user_status` does,
  rather than widening the underlying RLS policy — which stays narrow on
  purpose, so an ordinary self-service credential read never accidentally
  becomes admin-browsable.
- Not audited — a listing is a read, and the audit trail records actions
  taken; every action this list's UI triggers (invite, reissue, recovery)
  already writes its own row via the existing `/auth/invites` endpoints.

## [1.4.0] - 2026-08-04

Phase 1 hardening is complete. Every product tool route now requires a
real session, closing the last gap between "auth exists" and "auth is
enforced" — and TOTP, which shipped last release as a login-fallback
factor, has been repurposed into step-up verification for sensitive
in-session actions instead, once admin-driven account recovery made the
gap it was plugging redundant. See `AUTHENTICATION.md` for the updated
architecture and roadmap.

### Added
- **Every tool route now requires `AuthenticatedUser`** — upload, discover,
  validate, correct, correct-group, exempt-dimensions, exclude-group(s),
  analyze, export, the group-file/unit-file selection and confirmation
  endpoints, and session cancellation. Previously these were reachable by
  anyone who could reach the API at all; a session is now the minimum bar
  for touching any of them, matching what already held for every `/auth/*`
  endpoint.
- **Sessions record their creator.** `owner_id` on a tool session is
  stamped from the caller's `AuthenticatedUser` at the two points a session
  is actually created (`/upload`, `/dedup/check`) rather than left `None`.
  Captured for attribution, not access control — nothing yet enforces "only
  the owner may act on their own session", since every authenticated caller
  in this v1 shares the one `admin` role and no product feature needs
  narrower scoping yet. The column exists so that when a real ownership
  model is needed (a usage/activity log, a multi-role future), the data
  already exists back to this release rather than needing a backfill.
- **`POST /auth/totp/step-up`.** Given a fresh code from a *confirmed*
  authenticator app credential, elevates the caller's own session
  (`auth.sessions.elevated_until`) for five minutes. Scoped to the one
  session that presented the code — proving a code on one browser must not
  silently elevate every other device the same user is signed in on
  elsewhere.
- **`/health/whoami` now reports `totp_enrolled`.** Lets a caller (in
  practice, the frontend) show enrollment status accurately instead of
  always presenting "enroll", which would risk walking an already-enrolled
  user into silently replacing their working credential — re-enrolling
  overwrites the secret immediately, with no warning at the point of
  writing it.
- Two new audit events: `totp_step_up_succeeded` and `totp_step_up_failed`,
  replacing TOTP's participation in `login_succeeded`/`login_failed` now
  that it no longer signs anyone in.

### Changed
- **Adding a passkey to an already-signed-in account now requires step-up.**
  `POST /auth/register/begin`'s authenticated branch (add-a-passkey-to-
  yourself) refuses with `403 step_up_required` unless the session is
  currently elevated. Planting a durable new credential is exactly the
  kind of sensitive, high-blast-radius action step-up exists to gate — a
  hijacked session cookie alone must no longer be sufficient for it. The
  unauthenticated invite path is unaffected: token possession is already
  its own authorization there.

### Removed
- **`POST /auth/login/totp` — TOTP can no longer log anyone in.** It
  shipped last release as a fallback for a device with no passkey
  enrolled, reasoning that stopped holding once admin-driven account
  recovery (also last release) started covering "lost your only passkey"
  through a human-verified path instead. Keeping a self-service login path
  through a static, phishable shared secret — fully capable of
  authenticating alongside a hardware-bound passkey — meant the account's
  real security floor was the weaker of the two factors, undercutting the
  whole point of going passkey-first. The verification primitive itself
  (`verify_code`, the lockout columns and functions) is unchanged and is
  now step-up's own foundation instead.

## [1.3.0] - 2026-08-03

Phase 2 identity/session work is complete: all eleven originally-planned
steps now exist (bootstrap-admin, registration, login, invitations,
sign-out, TOTP fallback, and the deactivation/soft-delete cascades that
retire every access path they leave behind). Phase 1 hardening has also
begun — the unauthenticated auth endpoints and invite creation are rate
limited, a misconfigured deployment can no longer silently serve session
cookies over plain HTTP, a deliberate audit-coverage sweep closed three
rejection paths that wrote no row while a comparable one did, and an
admin can now recover an account that has lost its only passkey without a
password reset ever existing to lose in the first place.

**What is NOT here yet**: no frontend at all — no login page, no invite
redemption, no route gating. Auth exists and is fully exercised by a
backend test harness; nothing outside the auth endpoints themselves
requires a session yet, so this is not an enforced product. See
`AUTHENTICATION.md` for the full architecture, audit posture, and the
remaining roadmap.

### Added
- **TOTP as a fallback factor** — `POST /auth/totp/enroll/begin`,
  `/enroll/confirm`, `/disable`, and `POST /auth/login/totp`. A fallback for a
  device with no passkey, **not** a second step stacked on one: a passkey is
  already multi-factor and phishing-resistant, and requiring both would add
  friction to every sign-in in exchange for the weaker property.
  Authenticator apps only, never SMS.

  Enrolment is two steps and the second is the point — a secret is stored
  with `confirmed_at` NULL and only counts once a real code verifies.
  Otherwise a user could believe they had a working fallback while having
  mis-scanned the secret, and would discover it at the moment they needed it
  and had no other way in.

  **Encryption at rest, the decision the schema deferred to this task:**
  ChaCha20-Poly1305 with a 32-byte key from `TOTP_ENCRYPTION_KEY`. This is
  the one credential in the schema that cannot be hashed — the server holds
  the whole secret and must reproduce it on every verification — which is why
  the column was named `secret_encrypted` before anything could write to it.
  The ciphertext is bound to its `user_id` through the AEAD's additional
  data, so a secret grafted onto another user's row fails to decrypt rather
  than working. A version byte prefixes the blob so key rotation is possible
  later without guessing which ciphertexts are which.

  Labelled honestly as an **app-level stopgap**: the key lives in the
  environment, so a dump *plus* the key is as good as plaintext. What it
  defends is the realistic case — a leaked backup, a shared database branch,
  a logged query result. Real KMS stays trigger-gated.

  Sign-in is rate-limited (five failures, then a 15-minute lock) because a
  six-digit code is guessable in a way a passkey assertion is not. The lock
  is time-bounded and applies only to the fallback, so it cannot be used to
  deny someone their account — the passkey path consults none of it. **If
  TOTP ever becomes primary or mandatory, that reasoning stops holding and
  the lockout needs revisiting.**
- **Sign-out and sign-out-everywhere** — `POST /auth/logout` and
  `POST /auth/logout/everywhere`. Sessions were previously unrevocable and
  simply accumulated. Revocation goes through two new `SECURITY DEFINER`
  functions because `app_service` holds no `UPDATE` on `auth.sessions` and
  must not: a column grant would permit writing `NULL` as readily as a
  timestamp, handing the application an *un-revoke* primitive and defeating
  the reason an opaque session token was chosen over a JWT. Both functions
  can only move `revoked_at` from `NULL` to `now()`, so a replayed sign-out
  cannot even shift the timestamp to obscure when the real one happened.

  Both take a **token hash rather than a user id**, which makes them
  self-authorizing — they can only act on the account whose live token the
  caller actually holds, so "sign this other user out of everything" is not
  a request that can be expressed. Sign-out-everywhere additionally requires
  the presented session to be currently valid, so a leaked expired cookie
  cannot be used to sign someone out of every device.

  Neither endpoint sits behind the authentication extractor, deliberately:
  signing out must succeed with a stale or missing cookie, or the one moment
  a user most needs the cookie gone is the moment it 401s.
- **Invitation creation**, `POST /auth/invites`, admin-only. Creates the
  account as `invited` and returns a one-time token, or reissues for an
  account that already exists and has not enrolled yet — retiring any
  outstanding invite first, so at most one link is ever live per account.
  Needs no new database objects: the existing `users_insert_admin_only` and
  `user_invites_admin_only` policies already permit it under an admin
  identity, so the database enforces admin-ness independently of the
  handler's own check. `user_invites.created_by` populates itself from the
  identity GUC, which is what that column default was written for.

  Refusals mirror `bootstrap-admin --reissue-invite` exactly — an account
  with a passkey enrolled, or one not in `invited` status, is declined with
  a reason. Unlike the unauthenticated endpoints these say *why*: the caller
  is an administrator who can already list users, so withholding it protects
  nothing.

  No `role` field is accepted. Only `admin` exists, so accepting one would
  add a client-controlled path to choosing a new account's privilege level
  for no capability gained.
- Audit events now record **`target_user_id`**, not just the actor. Invite
  creation is the first event where the two are different people, and they
  are passed as a named `Subjects` value rather than two adjacent
  `Option<Uuid>` parameters — a transposition there would misattribute an
  administrative action to the person it was performed on, compile cleanly,
  and look entirely normal in the row.

  **Consequence worth knowing:** because both audit foreign keys are
  `RESTRICT` and the table is append-only, an invited account becomes
  permanently un-hard-deletable the moment an invitation is issued for it.
  Previously that only happened once someone *did* something. A mistyped
  address therefore leaves a permanent row that can be soft-deleted but
  never removed.

- **Invitation acceptance.** An invited user enrols their first passkey by
  presenting the token from their invitation link to
  `POST /auth/register/begin`, and finishes signed in. Eligibility is
  enforced entirely inside a new `auth.resolve_invite_registration`
  SECURITY DEFINER lookup — the invite must be unused and unexpired, the
  user must still be `invited`, and they must hold zero credentials — so an
  anonymous caller can neither enumerate users nor enrol over an existing
  credential. The invite is consumed at `/finish`, after the credential
  verifies, in the same transaction that writes it: cancelling the
  authenticator prompt therefore costs nothing, leaving the user `invited`
  with a live invite and a retry that just works.

### Removed
- **`AUTH_BOOTSTRAP_ENABLED`, and the unauthenticated bootstrap enrolment
  path it gated.** Deleted rather than left unset — setting it now does
  nothing. It keyed first-passkey enrolment on an email address, so the
  endpoint was answerable by anyone who could guess one, with an
  environment variable as the only thing standing in the way. Possession of
  an unguessable invite token replaces it, which cannot be accidentally
  switched on by a misconfigured deployment.
- `auth.resolve_bootstrap_registration`, dropped in the same migration.
  Leaving it would have left a callable SECURITY DEFINER function matching
  any active user with no credentials by email alone, with its only guard
  removed.

  The first administrator is unaffected: `bootstrap-admin` already creates
  them as an `invited` user holding an invite, so they now walk the same
  enrolment route as everyone after them instead of a special case that
  runs once and is never exercised again.

### Fixed
- **Deactivating an account now revokes its sessions.** The deactivation
  trigger removed passkeys and TOTP secrets, and (as of the previous change)
  retired pending invites, but left live sessions alone — while being named
  after revoking every access path. Not exploitable, because
  `auth.resolve_session` already requires `status = 'active'` and
  `deleted_at IS NULL`, so a deactivated user's token resolved to nothing.
  What existed was a row that looked live and was not, which misleads anyone
  asking "who is signed in right now". Includes a backfill for accounts
  deactivated before this covered them.
- **The session cookie was not actually being cleared in a browser.**
  Clearing emitted a `Set-Cookie` with **no `Path`**, which per RFC 6265
  defaults to the requesting URI's directory rather than "everywhere" — so
  logging out at `/auth/logout` produced a deletion scoped to `/auth`, which
  never matched the real cookie's `Path=/`. Nothing was exposed (the session
  is revoked server-side) but the browser kept presenting a dead token, so
  every later request 401'd with a cookie attached.

  It survived because the existing test asserted "the cookie no longer reads
  back" against an in-memory jar, which models no path semantics at all and
  passes either way. Clearing now also *adds an expired cookie* rather than
  removing an entry, because removal is a no-op unless the cookie was parsed
  from the request — the case where a browser holds a cookie the server did
  not receive is exactly when telling it to drop one matters most. New tests
  assert the emitted header's attributes, which is the only part a browser
  consults.
- A refused passkey registration is now recorded. Previously a
  `403 registration_not_available` wrote no audit row and emitted no log
  line at all, while a failed *login* wrote a `login_failed` row -- so
  probing registration across a list of addresses was untraceable while
  the identical probing against login was recorded. That asymmetry was an
  oversight, not a policy. Refusals now write a `registration_failed`
  audit row naming the reason (`bootstrap_disabled`, `missing_email`,
  `not_eligible`) plus the attempted address, and log a `warn`. The HTTP
  response is unchanged and still byte-identical across every reason, so
  the endpoint remains useless for user enumeration -- what an attacker
  cannot distinguish and what an operator cannot see are separate
  properties, and only the first was ever intended.
- A registration whose credential fails verification also writes a
  `registration_failed` row (reason `credential_rejected`), matching
  login's existing `assertion_rejected`.
- Three more of the same asymmetry, found in a deliberate audit-coverage
  sweep rather than by accident: passkey `login_begin` wrote no row for an
  empty/whitespace email while the very next case (an address that fails
  to resolve to a credential) already logged `login_failed`; TOTP login's
  combined `email.is_empty() || !totp_configured()` check was one
  unaudited early return covering two distinct reasons, now split so each
  logs its own (`empty_email` / `totp_not_configured`); and an admin's
  attempt to re-invite an already-credentialed or wrong-status account
  produced a `tracing` line and nothing permanent. All three now write an
  audit row — the last one under a new `invite_refused` event, the
  refusal type carrying a structured reason rather than only a free-text
  message.

### Added
- Both halves of a WebAuthn ceremony now log a shared `correlation_id`,
  and it is recorded in the audit metadata of every ceremony outcome. Two
  concurrent ceremonies for the same user were previously
  indistinguishable in the log, since both lines carried only `user_id`.
  This is a *separate* id from the ceremony's own, deliberately: the
  ceremony id is the contents of the ceremony cookie, so logging that
  would put a live bearer value into ops output.
- `passkey_registered` audit metadata now records `device_bound` as
  reported by the authenticator at enrolment, rather than leaving it to be
  read off the credential row later.
- **Rate limiting on the unauthenticated auth endpoints and on invite
  creation.** The single biggest live gap flagged by both an internal
  review and external LLM review of AUTHENTICATION.md. `tower_governor`
  (an in-process token-bucket limiter over the `governor` crate) rather
  than a hosted or edge rate-limiting service, matching the
  library-over-service preference already established for the rest of
  auth. One shared bucket covers passkey register begin/finish, passkey
  login begin/finish, and TOTP login — deliberately one bucket for all
  five rather than one each, so a script cannot multiply its budget by
  spreading attempts across endpoints. Invite creation gets its own,
  more generous bucket, verified genuinely independent of the first with
  a real test rather than assumed. Keyed by real TCP peer address
  (`axum::serve`'s `ConnectInfo`), not a client-supplied header — there is
  no trusted-reverse-proxy policy yet, so behind a proxy that does not
  preserve the real peer this still limits correctly, just coarsely.
- **Admin-mediated account recovery**, `POST /auth/invites/recover`. An
  admin can now revoke every existing access path on an already-active
  account (passkeys, TOTP, live sessions, any outstanding invite) and
  issue a fresh invite in its place — the piece that makes "someone lost
  their only passkey" actually recoverable rather than only described.
  Deliberately its own endpoint, not a flag on invite creation: the two
  operations have very different blast radii if triggered by accident.
  Reuses the existing deactivation trigger by cycling the account's
  status through `deactivated` and back to `invited` inside one
  transaction, rather than writing a second copy of the credential/
  session/invite cleanup those migrations already implement. Backed by a
  new `auth.set_user_status` function — the first `SECURITY DEFINER`
  function in this schema whose safety depends on checking the caller's
  *role* rather than being scoped by a token or the caller's own id, since
  there is no such scoping available for "an admin changes someone else's
  status."

### Security
- `SESSION_COOKIE_SECURE=false` (the local-HTTP-dev escape hatch) could
  reach a real deployment silently — nothing checked it against
  `WEBAUTHN_RP_ORIGIN`. The server now refuses to start with that
  combination paired with a non-localhost origin, alongside the other
  fatal misconfiguration checks (database pool, WebAuthn backend).

## [1.2.0] - 2026-07-29

Passkey registration and sign-in work end to end. A user with a record in
`auth.users` can enrol a passkey and then authenticate with it, receiving a
session cookie later requests are verified against. Confirmed against a
real browser and real authenticators (Windows Hello, and independently
Proton Pass) talking to a real Postgres branch, not only in tests.

**What is NOT here yet**, since "auth works" would overstate it: no
sign-out, no invitation flow, no first-admin bootstrap beyond the env-gated
path below, no TOTP fallback, no admin UI. Enrolling the very first passkey
for an account still requires `AUTH_BOOTSTRAP_ENABLED`, which must stay
unset in any environment that matters. Five of the eleven planned
identity/session steps are done.

### Added
- Postgres connectivity via sqlx, connecting as a dedicated app_service
  role rather than the migration/owner role, so row-level security actually
  applies to application traffic. DATABASE_URL configures the connection
  pool, built lazily so a missing or incorrect credential does not block
  application startup. GET /health/db reports connectivity and confirms
  which role the pool is actually authenticating as.
- AuthBackend trait plus a webauthn-rs-backed implementation
  (WebauthnRsBackend), held in AppState behind Arc<dyn ...> the same way
  the existing session stores are, per the standing interface-first design
  rule.
- Session cookie plumbing: opaque token generation and hashing
  (session_token.rs) and httpOnly/Secure/SameSite cookie issuance, reading
  and clearing (session_cookie.rs). Deliberately unsigned and unencrypted --
  the cookie carries an opaque random token only ever trusted after a
  database round-trip, never decoded as a claim.
- AuthenticatedUser, an axum extractor resolving the session cookie into a
  verified user id and role via resolve_session(), plus
  begin_rls_transaction for handlers running further RLS-scoped queries
  under that identity. GET /health/whoami exercises the chain end to end.
- POST /auth/register/begin and /auth/register/finish. An authenticated
  caller enrols an additional passkey for themselves, taken from their
  session -- any email in the body is ignored, since honouring it would let
  a signed-in user write a credential onto another account. Otherwise the
  request falls to an env-gated bootstrap path, which exists only because
  nothing can sign a user in before a first credential exists.
- POST /auth/login/begin and /auth/login/finish. Verifying an assertion
  persists the credential state the ceremony advanced along with
  last_used_at; a frozen stored value would make the anti-cloning check
  pass indefinitely on authenticators that do implement a counter.
- Audit-event recording (auth/audit_log.rs) for login_succeeded,
  login_failed and passkey_registered, wired in from the start rather than
  switched on later so the record has no gap. Recording is deliberately
  infallible to callers: propagating a logging failure would let anyone who
  could break audit writes deny logins.
- SECURITY DEFINER lookups behind the unauthenticated paths
  (resolve_bootstrap_registration, resolve_login_candidate) enforcing
  eligibility in the database rather than in a handler, so a future
  endpoint that forgets to check cannot become a hole. Each answers every
  ineligible case identically, so neither can be used to discover which
  addresses have accounts.

### Security
- app_service held table-level UPDATE on auth.users, and
  users_update_own_or_admin is row-scoped rather than column-scoped, so a
  caller could have updated their own row with `SET role = 'admin'`. Not
  reachable today only because every existing user is already admin; it
  would have become live the moment a second role existed. UPDATE is now
  granted on first_name, last_name and job_title only -- role, status,
  company, email, deleted_at and deletion_reason are administrative and
  must go through a SECURITY DEFINER function that checks the caller.
- app_service likewise held UPDATE on auth.sessions, where the same
  row-scoped policy would have let a caller clear their own revoked_at --
  undoing "sign out everywhere" -- or extend expires_at indefinitely. Both
  defeat the reason an opaque token was chosen over a JWT: revocation that
  is instant and complete. The grant is revoked outright with no
  column-level replacement, since every sanctioned session mutation already
  runs through a SECURITY DEFINER function.
- scripts/setup_app_service_role.sql silently undid the auth.users fix.
  Its blanket `GRANT ... ON ALL TABLES IN SCHEMA auth` re-granted
  table-level UPDATE, so running a script documented as safe to re-run
  reopened the escalation vector with no error and no output. It now
  re-asserts the narrow grants.
- UPDATE and DELETE on auth_audit_logs are revoked from app_service,
  completing the append-only intent. A third barrier rather than a hole
  closed -- RLS default-deny and the append-only triggers already blocked
  both -- but it does not depend on policy evaluation being configured
  correctly, which is worth having on the one table whose whole value is
  being untamperable.

### Fixed
- The application could not talk to Neon's pooled endpoint at all. db.rs
  set `search_path` as a connection option, which travels in the Postgres
  startup packet and is rejected by the pooler ("unsupported startup
  parameter in options: search_path"), failing every query including
  /health/db. All application SQL is now schema-qualified and no
  search_path is set. Moving it to a per-connection SET would not have
  worked either: the pooler is transaction-mode, so a session-level SET is
  not reliably bound to the client that issued it and would have started
  leaking under concurrency. Not caught earlier because the unit tests use
  an unreachable lazy pool and execute no SQL, and because a direct
  connection accepts the parameter happily -- identical code worked or
  failed purely on which endpoint DATABASE_URL named.
- webauthn_credentials.device_bound is now written from the credential
  rather than left to the column's DEFAULT true, which had every row
  asserting the key could not leave its hardware. The first real passkey
  was backup-eligible -- a synced credential -- while its row said
  otherwise. No security decision reads the column, so nothing was
  bypassable; the value was simply false, and the planned admin
  enrolled-factor view would have shown it as fact.
- scripts/setup_app_service_role.sql could not bootstrap a fresh branch in
  any order: the role must exist before migrations run, because the RLS
  migrations end with GRANT EXECUTE to it, but its grants can only be
  applied after, since the schema and tables do not exist until then. Every
  schema- and table-dependent statement is now guarded, so the file is safe
  to run at any point -- run it, migrate, run it again.

### Changed
- Requiring device-bound (non-syncable) passkeys for accounts holding
  third-party credentials is dropped rather than deferred. Enforcing it
  would reject what Windows Hello and password managers produce by default,
  and break working from more than one machine, to protect secrets that do
  not exist yet. device_bound is recorded for visibility; nothing refuses a
  credential on it.
- The shared test pool's acquire_timeout is 50ms instead of sqlx's 30s
  default. The pool is lazy, so a handler path that unexpectedly reaches
  the database does not error -- it stalls for the full timeout and then
  errors, leaving the test passing and only the suite slower. Five login
  tests took 30.00s between them before this; they now take 0.05s and an
  unintended query fails fast instead of hiding.

## [1.1.5] - 2026-07-29

A fresh adversarial review pass (5 parallel reviewers, one per crate/
layer boundary) after 1.1.4 shipped, run to close out the refactor
before a code-quality conclusion. No new functionality.

### Fixed
- `Session::complete_discovery` didn't bump `data_generation`, reopening
  the exact class of race 1.1.3/1.1.4 closed for corrections/exemptions/
  exclusions: handlers reachable after `Analyzed`/`Exported` (unit-file
  format resolution, group-file selection) mutate `SessionData` directly
  without going through a generation-bumping method, so a change landing
  in `/analyze` or `/export`'s read -> write-back gap could still have
  its safety-net stage downgrade silently re-promoted. Fixed at the
  single funnel (`complete_discovery`) rather than each handler.

### Changed
- Moved `find_typo_variant_candidates` from `report.rs` into
  `similarity.rs`, matching this crate's own documented one-module-
  per-signal convention (next to `relatedness.rs`'s equivalent).
- Corrected `dedup/RULES.md`'s "individually well-formed email" wording
  to match what `all_emails_present_and_distinct` actually checks
  (non-blank and mutually distinct -- no format validation).
- Removed two no-op entries from `STREET_SUFFIXES`.

### Added
- A regression test for the `complete_discovery` generation-bump fix.
- Completeness tests for `FIELD_SPECS`/`CATEGORY_PRIORITY` against every
  `FieldName`/`FieldCategory` variant (compile-error-on-drift, via an
  exhaustive match with no wildcard arm).
- Tests exercising `GroupCheckAcknowledgments` with real (non-default)
  values at the `unit-group` crate's own unit-test level.
- A zero-row dedup pipeline test.

## [1.1.4] - 2026-07-28

Closes out the two file splits and two concurrency gaps this pass's own
prior audits had deferred, plus the remaining flagged test-coverage
gaps and a dead code-path removal. No new functionality.

### Fixed
- Session-cleanup sweep held the entire session map's write lock for
  its full O(n) scan, blocking every concurrent `save`/`get_handle`/
  `delete` call for the whole sweep, not just the sessions actually
  being removed. Now scans for expired candidates under a read lock,
  then takes the write lock only to remove them, re-verifying each is
  still expired immediately before removal.
- `cancel_session`'s concurrent-mutation race (previously only logged,
  not fixed, in 1.1.3): a concurrent handler already holding its own
  handle to a session could still complete a mutation on it after
  `cancel_session` removed it from the map, with no way for any future
  caller to ever observe that write. Fixed with a `cancelled` flag set
  under the session's own write lock before removal; every generic
  session-access method now treats a cancelled session exactly like a
  nonexistent one, mirroring the existing owner-mismatch gate -- no
  individual handler needed changes.
- Removed the `acknowledge_errors` export override -- dead code with
  no reachable UI path (the frontend's own "Continue" button is
  disabled until every issue, not just Errors, is already resolved, so
  the override could never fire). Every real `Severity::Error` issue
  type already has inline correction UI; the one condition that stays
  unconditionally blocking, a file that failed to parse, correctly
  should.

### Changed
- Split `api/validate.rs`'s summary-building logic into
  `validate/summary.rs` (285 -> 211 lines).
- Split `discover/compute.rs`'s selection logic into
  `discover/selection.rs` (335 -> 176 lines).

### Added
- Regression tests for both concurrency fixes above, a Unicode/
  diacritic name-matching test, two smallest-input pipeline tests (1
  and 2 fabricated records), an error-shape sweep test across several
  endpoints, and an oversized-request-body test.

## [1.1.3] - 2026-07-28

12 real bugs found via a 6-agent adversarial review (core parsers, dedup,
unit-group, and the HTTP/session layer), each confirmed empirically
before fixing and each with its own regression test; no new
functionality.

### Fixed
- SpreadsheetML `<![CDATA[...]]>` cell values were silently dropped
  (no parser match arm for that event) instead of surfacing an error.
- Excel float cells silently saturated to `i64::MAX`/`MIN` for a
  whole-number value outside `i64`'s range instead of erroring.
- A blank phone-number prefix on one dedup record falsely flagged a
  Phone-category mismatch even when the actual phone number matched.
- `group_key` didn't collapse internal whitespace, so two records
  differing only by a double space landed in separate tenant groups.
- The typo-variant candidate sort used `partial_cmp(...).unwrap()`,
  a latent NaN panic path; switched to `total_cmp`.
- Dimension exemption was silently ineffective when a unit's UnitGroup
  name was itself a malformed dimension attempt (e.g. `"10x"`).
- Unit-number identifiers weren't trimmed consistently (asymmetric
  with UnitGroup), across validation, corrections, and `/correct-group`.
- Comma-decimal dimension values (`"10,5"`) were rejected as invalid.
- Repeating an identical `/correct-group` rename request returned 400
  the second time instead of succeeding as a no-op.
- A concurrent correction/exemption/exclusion landing between
  `/analyze` or `/export`'s read and delayed write-back could have its
  safety-net stage downgrade silently undone, re-promoting the
  workflow using stale pre-correction data -- confirmed live. Fixed
  with a session data-generation counter checked before each write-back.
- Malformed JSON, a wrong Content-Type, or an oversized body rejected
  with a plain-text response instead of this API's standard
  `{error, message}` shape.
- `/correct` and `/exempt-dimensions` accepted a `unit_number` that
  didn't exist in the file, or was ambiguous (shared by 2+ rows from
  an already-flagged duplicate), silently storing a dead or
  data-corrupting entry with no error.

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
