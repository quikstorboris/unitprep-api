# Authentication

UnitPrep is self-hosted, passwordless, and phishing-resistant by design.
There is no password anywhere in this system — not as a primary factor,
not as a fallback, not as a "just in case." Signing in means presenting a
passkey (Windows Hello, Touch ID, a hardware security key, or a
password-manager-backed credential); an authenticator-app code (TOTP) is
the only fallback, for a device that has no passkey enrolled yet.

This document describes what exists today, why it's built the way it is,
what it looks like to actually use, how it holds up under an audit, and
what's still left to build. Status: **backend implementation is
functionally complete** (WebAuthn registration/login, TOTP fallback,
sessions, invites, sign-out, audit logging). **The frontend does not yet
exist** — there is no login page, no invite-redemption page, and no
route gating in `unitprep-ui`. Nothing is enforced as a product yet; see
[Planned Development](#planned-development-to-finalize-auth).

## Technical / architectural description

### Identity and credentials

- **WebAuthn/passkeys are the primary and only mandatory factor**,
  verified server-side by [`webauthn-rs`](https://github.com/kanidm/webauthn-rs)
  (the Kanidm project's implementation — mature, audited via real
  production use elsewhere). The cryptographic ceremony
  (`navigator.credentials.create()` / `.get()`) has to run in the
  browser; the frontend's only job is relaying the raw ceremony bytes to
  the backend, which does all verification and decision-making.
- **TOTP (RFC 6238) is a fallback factor**, via `totp-rs` with
  `default-features = false` (no `qrcode`/`image` crates pulled in — the
  otpauth:// URI it returns is meant to be rendered as a QR code
  client-side, not server-side). TOTP secrets are encrypted at rest with
  ChaCha20-Poly1305, keyed by `TOTP_ENCRYPTION_KEY`, with the ciphertext
  cryptographically bound to the owning `user_id` via the AEAD's
  additional data (so a secret can't be grafted onto another row and
  decrypt successfully) and a version byte for future key rotation.
  Enrollment is two-step — `confirmed_at` stays `NULL` until a real code
  verifies — so a mis-scanned secret is caught at enrollment, not the
  first time it's actually needed. Step-up verification is rate-limited:
  5 failed codes locks the factor for 15 minutes. This lockout is only
  safe *because* TOTP is a fallback — the passkey path never consults
  it, so a lockout inconveniences the account owner rather than denying
  them entirely. If TOTP is ever promoted to a primary or mandatory
  factor, this reasoning breaks and the lockout becomes an
  account-denial vector.
- **Re-enrolling keeps the old factor live until the new one is proven
  (2026-08-04).** `auth.totp_credentials.pending_secret_encrypted` holds
  the candidate secret from `/enroll/begin`; the existing confirmed
  secret is untouched until `/enroll/confirm` verifies a code against
  the *pending* one, at which point it's promoted in the same statement.
  Before this, `/enroll/begin` overwrote the live secret immediately, so
  an abandoned re-enrollment left the account with no working step-up
  factor at all until it was finished. There is also no "disable TOTP"
  action any more — TOTP is step-up-only, never a login factor, so
  there was no security upside to letting an account have zero step-up
  factor, only a self-inflicted-lockout risk. Re-enrolling replaces a
  factor; it never removes one with nothing to replace it.
- **No SMS, ever.** Explicitly out of scope — phishable and
  SIM-swappable, with no upside over TOTP.
- **Which actions require step-up is admin-configurable, not hardcoded
  (2026-08-04).** `auth.auth_configuration.step_up_actions` (a JSONB
  array) is now the source of truth — the first, and so far only,
  entry is `add_passkey` (adding a new passkey to an account that
  already has one). Read via `auth::step_up_policy::action_requires_step_up`
  under the caller's own identity — `auth_configuration` was previously
  admin-only-readable under RLS, which would have made this check
  impossible for an ordinary (non-admin) caller checking their own
  action; a second, narrower RLS policy now permits `SELECT` for any
  authenticated caller, while `INSERT`/`UPDATE`/`DELETE` stay
  admin-only. `auth_configuration.mandatory_passkey_enrollment` was
  dropped the same day — no code path ever made passkey enrollment
  optional, so the column implied a control that didn't exist.

### Sessions

- A single `httpOnly`, `Secure`, `SameSite=Strict` cookie holds an
  **opaque random session token** — not a JWT. The cookie is never read
  or parsed by frontend JS; the token is meaningless without a database
  round trip. `SameSite=Strict` (tightened from `Lax` as Phase II item
  2) means the cookie is never sent on a cross-site request at all, not
  even a top-level navigation arriving from another site — the
  narrower CSRF surface costs nothing here, since no legitimate flow in
  this app depends on the session cookie surviving a cross-site
  navigation. Ceremony cookies (`unitprep_reg_ceremony`,
  `unitprep_login_ceremony`) carry the same attribute for the same
  reason.
- The database stores only a **SHA-256 hash** of the token, never the
  raw value — a database read alone can't produce a usable session.
- Session creation and lookup both go through `SECURITY DEFINER`
  Postgres functions (`auth.create_session`, `auth.resolve_session`),
  which also validate the user is still active and non-deleted on every
  request and bump `last_seen_at`.
- Two independent expiry clocks (Phase II item 2): an **absolute**
  ceiling (`SESSION_LIFETIME_HOURS`, default 12h) fixed at login and
  never extended, and an **idle** timeout (`SESSION_IDLE_TIMEOUT_MINUTES`,
  default 30) checked against `last_seen_at` on every request. Either one
  expiring makes `resolve_session` return no row, indistinguishable from
  a revoked or nonexistent session — a session that is merely idle-expired
  is never resurrected by a later request, since `last_seen_at` is only
  advanced for rows the same query's `WHERE` clause already matched.
- Revocation is instant and complete: `auth.revoke_session` and
  `auth.revoke_all_sessions_for_token` are both keyed by a **token
  hash**, not a user id, which makes them self-authorizing — "sign this
  account out everywhere" isn't a request the API can even express for
  an account other than the one whose live token was presented. The
  admin-facing "sign a specific user out" form belongs with the admin
  panel (planned), which checks the caller's role explicitly.
  `app_service`, the role the application connects as, holds **no
  `UPDATE` grant on `auth.sessions` at all** — not even a column-scoped
  one — so a revoked session cannot be un-revoked by any application
  code path, buggy or otherwise.
- Anomaly signal (Phase II item 4): a login from an IP or `user_agent`
  never seen before for an account with prior sessions sets
  `auth.sessions.requires_step_up`, unconditionally audited
  (`login_anomaly_detected`) and gated behind an immediate TOTP step-up
  when the account has one confirmed. `AuthenticatedUser` refuses every
  route except `/auth/totp/step-up` and `/health/whoami` while the flag
  is set; `auth.record_step_up` clears it the moment a fresh code
  verifies. See the Phase II list below for the full reasoning.
- Startup refuses to boot if `SESSION_COOKIE_SECURE=false` is paired
  with a non-localhost `WEBAUTHN_RP_ORIGIN` — a misconfigured
  deployment that would ship session cookies over plain HTTP fails fast
  instead of silently shipping.

### Data model and database-layer defense

- Postgres via Neon. All auth tables live in a dedicated `auth` schema:
  `users`, `webauthn_credentials`, `totp_credentials`, `sessions`,
  `user_invites`, `auth_audit_logs`.
- **Row-Level Security (RLS)** is enforced independently of application
  code — a bug in a Rust handler can't silently read or write rows it
  shouldn't, because the database itself refuses the wrong query shape.
- The application connects as `app_service`, a role with narrowly
  scoped grants rather than superuser/owner access. Anything requiring
  elevated privilege (revoking a session, changing a user's role or
  status, consuming an invite) is a `SECURITY DEFINER` function that
  checks the caller itself, rather than a broad table grant the
  application is trusted to use correctly.
- All application SQL is schema-qualified (`auth.users`, not `users`) —
  no `search_path` is set on the connection, which matters specifically
  because Neon's pooled connection mode doesn't reliably support
  connection-level `search_path` settings.

### Onboarding, invites, and enrollment

- There is no self-registration. An admin creates a user record and
  issues a single-use, hashed invite token, shared manually (Teams/
  Slack/etc.) — no email service is wired up for this, by design (see
  [Architectural Choices](#architectural-choices-and-reasoning)).
- Invite redemption is folded into the *same* WebAuthn registration
  endpoint as "add another passkey to my own account," as a third
  authorization path, so there is exactly one passkey-registration code
  path in the system rather than two that could drift apart. Every
  eligibility rule (invite unused and unexpired, user still in
  `invited` status, zero existing credentials) lives inside a single
  `SECURITY DEFINER` function, `auth.resolve_invite_registration` —
  which means an anonymous caller cannot use this endpoint to enumerate
  which email addresses have accounts, and cannot enroll a competing
  passkey over an existing one, regardless of what the HTTP handler
  does.
- The invite is consumed **after** the credential verifies, **in the
  same database transaction** as the credential insert. Both halves
  matter: consuming earlier risks a cancelled prompt leaving an
  `active` user with no credential and a spent invite; consuming later
  (non-atomically) risks the opposite. Either stranded state is
  currently unrecoverable by the existing tooling, so the transaction
  boundary is what actually prevents it — not application-level care.
- The very first administrator account goes through this exact same
  path (via a one-time `bootstrap-admin` CLI subcommand that creates an
  `invited` user with an invite, rather than a special unauthenticated
  HTTP route). There is no environment-variable-gated bootstrap
  endpoint in the codebase; that pattern existed early on and was
  deliberately deleted, not merely disabled.

### Audit logging

- `auth_audit_logs` is append-only: no `UPDATE` or `DELETE` grant
  exists for it, for any role, including admin-facing ones.
- Every registration attempt — success, verification failure, or
  outright refusal (bad/expired/already-used invite, account not
  eligible) — writes a row, via a shared helper
  (`auth::audit_log::record`) that every rejection path is routed
  through specifically so a new rejection reason can't be added later
  without also being logged. The HTTP response for every rejection
  reason is byte-for-byte identical (anti-enumeration); the audit row
  is written server-side regardless, which is a different property —
  "invisible to the attacker" and "invisible to the operator" don't
  have to be the same fact, and conflating them was an actual gap that
  got closed.
- Login, TOTP enrollment/verification/lockout, and session revocation
  are logged the same way.
- **Never use `INSERT ... RETURNING` against `auth_audit_logs`** — the
  table's `SELECT` policy is admin-only, and `RETURNING` is evaluated
  against it, so it fails precisely on the anonymous/no-identity events
  the audit trail exists to capture (failed logins, refused
  registrations). The shared `audit_log::record` helper already avoids
  this; any new direct write to the table has to as well.

### WebAuthn ceremony state

- The server-side state linking a WebAuthn ceremony's two HTTP
  round trips (`/begin` and `/finish`) is held **in memory**, referenced
  by a short-lived (5-minute) opaque cookie of its own — deliberately
  the same shape as the real session cookie, for the same reasons.
  This is correct for the security of the challenge itself, but it does
  mean ceremony state does not survive a process restart and does not
  work across more than one backend instance without a shared store.
  Not a problem today (single instance); flagged as something to
  revisit if horizontal scaling is ever needed.

## Architectural choices and reasoning

**Self-hosted, not a managed identity SaaS.** WorkOS and Clerk were
considered and explicitly rejected. The project distinguishes
third-party *services* (ongoing, hosted, billed, control ceded) from
third-party *libraries* (audited, open-source, self-hosted, no vendor
relationship) — and for anything on the security-critical path, prefers
a vetted library with no ongoing vendor dependency over a vetted
service. `webauthn-rs` fits that; a hosted identity provider doesn't.

**Passkeys as the primary factor, not "2FA bolted onto something
weaker."** A passkey is already inherently multi-factor (something you
have — the private key — plus something you are or know, via the
platform's own biometric/PIN unlock) and phishing-resistant, because
the browser cryptographically binds the credential to the real origin —
there's no code or token an attacker can trick someone into typing into
a look-alike site. TOTP doesn't have that property; a code can be
captured and relayed by an adversary-in-the-middle proxy in real time.
That's why TOTP is a fallback for un-enrolled devices, not a mandatory
second step stacked on top of a passkey.

**Opaque session token, not a JWT — final.** A JWT's whole appeal is
statelessness (no DB lookup per request), which directly conflicts with
the requirement to instantly and completely revoke a session — a
stolen-device "sign out everywhere." A JWT can't be revoked before its
natural expiry without maintaining a revocation list anyway, which
defeats the point of using one. An opaque token looked up in Postgres
on every request gives instant, complete revocation for free, at a
latency cost that's negligible at this project's scale.

**httpOnly cookie, not a bearer token in `localStorage`.** The usual
"cookies are bad" narrative is almost entirely about third-party
tracking cookies — a different concern from a first-party session
cookie. A bearer token stored in `localStorage` is JS-readable, which
means a single XSS bug anywhere on the page becomes trivial session
theft. An `httpOnly` cookie isn't readable by JavaScript at all, and the
frontend genuinely never needs to read it — it's presented
automatically by the browser and consumed only by the backend.

**DPoP was seriously considered and rejected; step-up re-authentication
was adopted instead.** RFC 9449 DPoP is specified for OAuth2 bearer
tokens carried in an `Authorization` header, not cookies — adopting it
would mean either reverting to bearer tokens, or having frontend JS
actively sign a cryptographic proof on every request, which
contradicts "the frontend never touches session mechanics." No mature
Rust DPoP implementation exists at the level of `webauthn-rs` either.
**DBSC (Device Bound Session Credentials)**, a Chrome-led emerging
standard, is arguably the *actually correct* fit for "bind a cookie
session to a device-held key" — but it isn't broadly cross-browser
supported yet, so it's being watched, not built against. The practical
stand-in: **step-up re-authentication** (a fresh passkey tap required)
for sensitive actions, once there are sensitive actions to gate (Phase
4 / not built yet). Worth remembering: DPoP's threat model is a
credential physically exfiltrated and replayed from a *different*
device later — it does **not** protect against live XSS abuse on the
original device. Neither the plain-cookie model nor a DPoP-protected
one defends against an attacker with code already running on the page.

**Synced passkeys are explicitly acceptable — reversed from an earlier
"device-bound required" decision.** Device-bound (TPM/Secure
Enclave/hardware key — private key physically cannot leave the
hardware) is the stronger property versus synced (iCloud Keychain,
Google Password Manager, Proton Pass — private key material is
encrypted and replicated across every device signed into that
password manager). Device-bound was originally going to be mandatory
for any account that would eventually hold third-party (QMS/Dropbox)
credentials. That requirement was dropped for two concrete reasons: the
primary user works remotely on occasion, so a credential that can't
leave one machine means routine lockouts, not just emergency ones; and
the very first real registration ceremony showed Windows Hello produces
a synced credential *by default* — so enforcing device-bound would have
rejected the ordinary path on the ordinary browser, to protect
credentials that don't exist in the system yet. The practical
consequence of "synced": the credential's security is bounded by the
password manager's own account security (master password, its own
2FA, its encryption-at-rest), not by hardware unclonability — a real
but currently acceptable tradeoff, revisit once the account actually
holds something worth that extra protection. `device_bound` is still
recorded accurately per credential (informational only, nothing is
rejected on it today) so an admin view of who's on synced vs.
hardware-bound passkeys is possible later without new instrumentation.

**Postgres over SQLite — locked in.** SQLite was seriously considered
(simpler ops, no separate service) and rejected once real scale and
two Postgres-specific capabilities were weighed: Row-Level Security
(DB-enforced per-role visibility, independent of the backend's own
checks — real defense in depth, but only if the connection strategy is
actually designed around it, since connecting as a superuser typically
bypasses RLS entirely) and JSONB (fits the platform's "extensible, not
a closed list" client-config data well). A SQLite file is also just a
file — anyone with filesystem access has everything; Postgres enforces
access at the database layer itself, independent of the application.

**Rejections are deliberately uninformative to the caller, but never
invisible to the operator.** Every WebAuthn registration/login
rejection returns an identical, generic response regardless of the
actual reason (bad invite, no such user, wrong credential, expired
token) — distinguishing them would turn an unauthenticated endpoint
into a tool for enumerating which email addresses have accounts. The
*actual* reason is still written to the audit log server-side, because
"indistinguishable to the attacker" and "invisible to the operator" are
different properties, and a system can have the first without paying
for the second.

## User-friendly description of auth workflows

*(The workflows below describe the backend behavior that exists today.
The pages and buttons that would make them clickable in a browser don't
exist yet — see [Planned Development](#planned-development-to-finalize-auth).)*

**Signing in.** No username, no password, ever. You click sign in, your
browser or password manager prompts you the way it always does —
Windows Hello, Touch ID, a fingerprint, a security key tap, or your
password manager's own unlock — and you're in. If the device you're on
doesn't have your passkey (a work laptop you don't normally use, say),
you can fall back to a six-digit code from your authenticator app
instead.

**Getting your account set up for the first time.** An admin creates
your account and sends you a one-time setup link (currently shared
directly, e.g. over Teams — there's no automated "welcome" email yet).
Opening the link walks you through creating your passkey — the same
prompt as signing in later, just for the first time — and once it's
done, you're signed in immediately. No separate "activate your account"
step.

**Setting up the backup code method (TOTP).** From your account
settings (once built), you'll see a QR code to scan with an
authenticator app (Google Authenticator, 1Password, Authy, etc.). You
confirm it worked by entering the six-digit code it shows you once —
this "confirm" step exists specifically so a mis-scanned code gets
caught immediately, rather than the first time you actually need the
fallback and discover it doesn't work.

**Locking yourself out five times in a row on the backup code.** After
5 wrong codes, the backup method locks for 15 minutes. Your passkey
still works normally the entire time — the lockout only ever affects
the fallback, never your primary way in.

**Losing your device / your only passkey.** There is deliberately no
"forgot password" style self-service reset — there's no password to
forget in the first place, and an automated recovery email would
introduce a weaker link than what you started with. Instead: you
contact an admin directly (a real conversation, not a form), they
verify it's really you the same way anyone would in a small
organization, and issue you a fresh setup link exactly like the one you
got when your account was first created. This is intentionally a human
step, not an automated one — losing your credential is rare enough that
adding a person to the loop costs little and closes off "I clicked a
link" as an entire class of attack.

**Signing out.** Signs out the device you're on. A "sign out
everywhere" option (revokes every session on every device at once) is
built on the backend and will be exposed once the frontend exists.

## Audit preparedness notes

### Already in place

- **Append-only audit trail.** `auth_audit_logs` has no `UPDATE`/
  `DELETE` grant for any role. Once written, an entry cannot be
  altered or removed by the application.
- **Full event coverage for the paths built so far**, including the
  anonymous/pre-authentication ones that are easy to overlook: login
  success and failure, passkey registration success/failure/refusal,
  TOTP enrollment/confirmation/removal/lockout, and session revocation.
  Each row carries actor, target (where one exists), user agent, and a
  structured reason — queryable fields, not prose.
- **No credential material ever touches the audit trail or logs.**
  Raw invite tokens, session tokens, TOTP secrets, and WebAuthn
  challenges are never recorded anywhere, including in rejection rows
  that name *why* something failed.
- **Everything sensitive at rest is hashed or encrypted, never
  plaintext.** Session tokens and invite tokens: SHA-256 hash only.
  TOTP secrets: ChaCha20-Poly1305, bound to the owning user, versioned
  for rotation.
- **Database-layer access control independent of the application.**
  RLS plus `SECURITY DEFINER` functions mean the database itself
  enforces who can read or write what, as a second layer behind
  (not instead of) the API's own checks.
- **Least-privilege application role.** `app_service` cannot `UPDATE`
  `auth.sessions` at all, cannot change a user's `role`/`status`/
  `email`/`deleted_at`, and cannot select from `auth_audit_logs` beyond
  what an admin identity is allowed to see. Privileged mutations are
  narrow, named, `SECURITY DEFINER` functions, each with a single
  documented purpose, rather than broad table grants trusted to be used
  correctly.
- **No hard delete, anywhere, for a user with audit history.**
  Deactivation is soft — status changes, credentials are removed by
  trigger, history is retained. This is intentional and matches what
  append-only audit expectations (SOC 2 / PCI / HIPAA / SOX-adjacent)
  normally require.
- **Anti-enumeration by design**, without sacrificing operator
  visibility — see [Architectural Choices](#architectural-choices-and-reasoning)
  above.

### Planned — needed before this is genuinely audit-ready

- **Formal audit-coverage verification** (in progress) — a closing
  pass confirming every auth event type that *should* produce a row
  actually does, rather than relying on it having been true so far.
- **Rate limiting on unauthenticated auth endpoints.** TOTP already has
  lockout; passkey registration/login and invite redemption currently
  do not, which is a real gap an external reviewer would flag.
- **A formal threat model / control matrix.** What exists today is a
  careful, defensible design — it has not yet been written down as a
  structured document mapping specific controls to specific threats,
  which is typically what an external audit actually asks for.
- **Documented retention and review process for the audit log.**
  Good mechanics (append-only, hashed, complete) is only half of
  audit-readiness; the other half — how long entries are kept, who
  reviews them and how often, separation of duties — is process, not
  code, and doesn't exist as a document yet.
- **Key management beyond an environment variable.** `TOTP_ENCRYPTION_KEY`
  is an explicit, documented stopgap: a leaked process environment is
  as good as plaintext for every TOTP secret it protects, and there is
  currently no rotation path (the ciphertext format already carries a
  version byte specifically so rotation can be added later without
  guessing which ciphertexts are under which key).
- **A right-to-erasure / anonymize path.** Today, personal data
  (email, name) on a deactivated account is retained indefinitely
  alongside its audit history — appropriate for the current scale, but
  not GDPR/CCPA "right to erasure" compliant. Deliberately deferred
  until an actual trigger (first EU-based user, first enterprise
  security review, first customer DPA) rather than built speculatively.
- **An external, adversarial security review.** Everything above is
  the result of careful design and code review — reasoning about the
  system, not someone actually trying to break it. A scoped
  penetration test is the highest-leverage single action left for
  external audit credibility, precisely because it's the one thing on
  this list that isn't just more careful reasoning.

## Planned development to finalize auth

Externally reviewed 2026-07-31 (Grok, Copilot) against this plan. Both
independently converged on the same shape we already had — rate
limiting and route gating as the highest-leverage remaining gaps, the
recovery and frontend work correctly sequenced after them — which is
useful confirmation on its own. Two concrete gaps surfaced that weren't
previously written down anywhere (items 4 and 6 below), and one item
below is already done rather than pending.

**Already shipped, not just planned:** the `SESSION_COOKIE_SECURE`
fail-fast startup guardrail (refuses to boot with a non-localhost
origin and an insecure cookie) — see [Sessions](#sessions) above.

### Phase I — ship it, enforce it, close the obvious gaps

1. Finish audit-event coverage verification (the closing check above) —
   specifically confirm every rejection path, not just the common ones,
   actually routes through the shared audit helper rather than assuming
   it does because the common ones do.
2. Add rate limiting to the remaining unauthenticated auth endpoints
   (registration, login, invite redemption) — and to invite *creation*
   too, even though it's authenticated, since it's still a path that
   can be hammered.
3. Extend invite issuance to support the account-recovery case — right
   now, re-issuing an invite is refused for any account that already
   holds a credential, which means there's currently no path back in
   for someone who's already enrolled and loses their device. This is
   the piece that makes the admin-mediated recovery workflow described
   above actually possible, not just described.
4. **Decide the tool-session ownership rule before gating routes, not
   after.** `owner_id` already exists on the `unit_group`/`dedup`
   session model and is currently always `None` — tool sessions and
   auth sessions are deliberately parallel systems today, with no
   relationship between them. Once every request carries an
   `AuthenticatedUser`, that stops being a neutral default: either a
   tool session gets stamped with its creator's `owner_id` and becomes
   access-checked (only its owner, or an admin, can reach it), or it
   stays intentionally shared/anonymous and that choice gets written
   down as deliberate. Left implicit, this is exactly the kind of thing
   that becomes a messy retrofit once real users and real data exist in
   those sessions.
5. Gate the product's actual routes behind requiring a valid session —
   today, auth exists but nothing outside the auth endpoints themselves
   requires it. This is the step that makes item 4 unavoidable, which
   is why 4 is sequenced immediately before it rather than after.
6. **Decide the role model before building the admin panel, not
   during it.** `Role` is already structured to be more than a single
   value, but nothing beyond `Admin` is meaningfully used anywhere yet.
   The Users panel (item 8) will force this question one way or
   another — who can view the user list, who can create invites, who
   can revoke someone else's session — so it's worth deciding
   deliberately up front (even if the answer is "just Admin, for now,
   and here's why") rather than letting the first admin-panel PR decide
   it by accident.
7. Build the frontend, essentially from scratch: a WebAuthn client
   integration, a login page, an invite-redemption page (which doubles
   as the recovery re-enrollment page), TOTP enrollment and login UI
   (including rendering the otpauth:// URI the backend already returns
   as an actual QR code), a signed-in-user context, route-gating
   middleware, and a sign-out control. While building this, keep the
   request/response shapes for the auth endpoints (WebAuthn ceremony
   payloads, TOTP enroll/verify bodies, the session/user shape) defined
   in exactly one place both sides can check against — even something
   as lightweight as a hand-maintained shared types file — rather than
   letting the frontend infer shapes from the backend's JSON and drift
   over time. This is the cheapest point to start that discipline,
   since both sides are being written new right now rather than one
   already existing and the other reverse-engineering it.
8. Build the admin Users panel: create invites, list users, manage a
   user's active sessions (including remote sign-out), deactivate an
   account, and correct a mistyped invite email. Can proceed in
   parallel with item 7 once items 1–6 are solid — they don't block
   each other.
9. Confirm the first-administrator bootstrap path is usable as a
   break-glass mechanism if the sole current admin ever loses their own
   device — and go one step further than just the CLI working: write
   down, briefly, where the pieces someone would actually need to use
   it live (`TOTP_ENCRYPTION_KEY`, database credentials, hosting/Neon
   account access) and who besides the current admin could reach them.
   Low priority, but "the CLI still works" and "break-glass access
   doesn't quietly depend on one specific person being reachable" are
   two different claims, and only the first is currently confirmed.

### Phase II — hardening

Scoped 2026-08-04: hardware-bound passkeys (item 1), real KMS (item 3),
and the formal external pentest (item 5) are deferred indefinitely — 1
pending a team decision on whether it's needed at all, 3 pending
willingness to take on a cloud-provider dependency, 5 as very low
priority with no confirmed need yet. Items 2, 4, 6, 7 were approved and
have since shipped. Item 8 was scoped and deferred the same day — see
its own entry below for why. **Phase II is closed out** for now: nothing
left on this list is scheduled, only trigger-gated.

1. ~~An optional policy requiring hardware-bound (security-key-only)
   passkeys, at least for admin accounts~~ — **deferred**, scope
   (everyone / admins-only / voluntary) undecided by the team.
2. **Shipped 2026-08-04:** session and TOTP hardening — idle
   (`SESSION_IDLE_TIMEOUT_MINUTES`, default 30) plus absolute
   (`SESSION_LIFETIME_HOURS`) session expiry, `SameSite=Strict` on the
   session and ceremony cookies, and a TOTP replay window
   (`auth.totp_credentials.last_used_step`) that refuses a code already
   accepted at that step or earlier. See [Sessions](#sessions) above and
   `auth::totp`'s module docs.
3. ~~Real key management (KMS or a self-hosted equivalent) for
   `TOTP_ENCRYPTION_KEY`~~ — **deferred**, not taking on a cloud-provider
   dependency (or standing up self-hosted Vault) at this point.
4. **Shipped 2026-08-04:** anomaly / risk-based signals. A login is
   flagged when the account has prior session history but this login's
   IP or `user_agent` matches none of it — audited unconditionally
   (`login_anomaly_detected`) and, when the account has TOTP confirmed,
   gated behind an immediate step-up: `auth.sessions.requires_step_up`
   is set at login and `AuthenticatedUser` refuses every route except
   `/auth/totp/step-up` and `/health/whoami` until it clears. An account
   with no TOTP confirmed is audited but never gated — there is no
   factor to step up with, and forcing a lockout over a self-service
   factor nobody set up would be a denial-of-service on that account,
   not a hardening measure. "Unexpected location" is scoped down to "new
   IP" rather than true geolocation, which would need a GeoIP database
   as a new dependency — deliberately deferred, easy to layer on top of
   `ip_address` later without another schema change. Direct exposure
   (no reverse proxy) is this deployment's actual topology today, so the
   raw TCP peer address (`ConnectInfo`) is the trusted IP source; revisit
   alongside a trusted-forwarded-header policy if a proxy/CDN (e.g.
   Cloudflare) is ever put in front of this service.
5. ~~A formal external penetration test~~ — **deferred**, very low
   priority, not confirmed this project will ever need one. Still the
   single highest-leverage move for external-audit credibility if a
   demanding audit is ever required.
6. **Shipped 2026-08-04:** the formal threat model / control matrix —
   see [THREAT_MODEL.md](THREAT_MODEL.md). Inventories every threat this
   auth system defends against, the control that closes it, and — named
   explicitly rather than left implicit — every deferred item and known
   gap from this list.
7. **Shipped 2026-08-04:** audit retention & review process
   documentation — see [AUDIT_RETENTION.md](AUDIT_RETENTION.md).
   Retention is indefinite by default (and structurally so: the
   append-only triggers on `auth.auth_audit_logs` block deletion
   outright, not just by convention); review is trigger-driven off
   specific event types (`login_anomaly_detected`,
   `account_recovery_initiated`, TOTP lockouts) plus a quarterly
   baseline pass, with runnable queries for both.
8. ~~A fix for ceremony state being in-memory and single-instance~~ —
   **deferred 2026-08-04**, scoped and explicitly not built: no
   multi-instance deployment is planned, and a real fix isn't a small,
   ceremony-specific change — `AppState` holds four separate
   `InMemorySessionStore` instances (the two WebAuthn ceremony stores
   this item names, plus the unit-group and dedup tool-session stores),
   all with the identical single-process limitation, so doing this
   properly means replacing the store backend (Redis, or Postgres-backed)
   everywhere at once, not just for ceremonies. Also arguably a downgrade
   for single-instance security in the meantime: WebAuthn challenge state
   never leaves the process today, which a shared external store would
   change. Revisit only if horizontal scaling actually becomes necessary.

### Not scheduled — revisit only if triggered

- Real email/ESP integration (blocked on choosing a provider, and not
  currently needed — recovery and invites work without it).
- Cloudflare Access / ZTNA (blocked on a hosting decision that hasn't
  been made).
- A right-to-erasure/anonymize path (blocked on an actual EU user,
  enterprise review, or data processing agreement — see above).

### Noted, deliberately outside this document's scope

The 2026-07-31 external reviews also raised a few real observations
that aren't auth-specific enough to belong in an auth roadmap, so
they're recorded here rather than silently dropped:

- **Dual naming** (`unit-group`/`UnitGroup` in code vs. "Group Prep"
  product-facing) is a real, if cosmetic, source of friction for a new
  contributor. Platform-wide naming, not an auth concern.
- **Tool-session client data lives only in browser `sessionStorage`.**
  Fine for the current single-tab, advisory-only tools; would need a
  real store if a "Client Prep navigation" concept persisting across
  tools and sessions is ever built. Platform feature work, not auth.
- **No OpenAPI / generated client for the API generally.** The
  lightweight shared-types discipline suggested for the auth surface in
  Phase I item 7 addresses the highest-drift-risk part of this; the
  rest of the API is an existing, working, unreviewed-here surface.
- **Single-maintainer risk** (raised by Copilot as the project's biggest
  actual risk, ahead of anything technical). Real, and not something an
  auth document can fix — it's the reason Phase I item 9 above was
  widened from "the CLI works" to "someone besides the current admin
  could actually reach what break-glass needs," but the broader version
  of this observation is an organizational question, not an engineering
  one.
