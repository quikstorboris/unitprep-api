# Audit Log Retention & Review

Phase II hardening item 7. Formalizes how long `auth.auth_audit_logs`
lives and who is expected to actually look at it — see
[THREAT_MODEL.md](THREAT_MODEL.md) for why the audit trail is treated as
an asset in its own right, and `src/auth/audit_log.rs` for what gets
written and why.

## Retention: indefinite today, and structurally so, not by policy alone

`auth.auth_audit_logs` has no deletion mechanism at all right now.
`prevent_audit_log_mutation()` raises on both `UPDATE` and `DELETE`
(migration `20260721194830`), so **no application code path — not a bug,
not a compromised `app_service` credential — can shorten retention**.
That is a stronger guarantee than "we don't currently delete anything":
it means retention today is a structural fact of the schema, not a
policy that could be silently weakened by a future change that forgets
to check.

**Policy: keep everything, indefinitely, until a concrete reason to do
otherwise exists.** At this system's actual scale (a handful of admin
users, auth events only — not a high-volume consumer product), storage
cost is immaterial; the value of a long-tail security investigation being
able to reach back further is not. This is a default, not a permanent
decision — revisit if any of the following actually happens, rather than
on a calendar:

- A compliance requirement is identified that mandates either a maximum
  retention period or a minimum one (both exist in different
  regulatory regimes — check which applies before assuming "shorter is
  safer").
- Table size becomes operationally relevant (backup time, query
  performance against the `created_at` index) — not expected for a long
  time at this event volume, but worth a periodic sanity check (`SELECT
  count(*), pg_size_pretty(pg_total_relation_size('auth.auth_audit_logs'))
  FROM auth.auth_audit_logs;`) rather than assuming.
- The deferred right-to-erasure/anonymize path (tracked as its own
  trigger-gated initiative, out of both Phase I and Phase II — see the
  vault's auth hardening two-phase-plan note) is actually built. Note
  that a deleted user's audit rows already **survive** the deletion
  today: `actor_user_id`/`target_user_id` are `ON DELETE SET NULL`, not
  `CASCADE` — the event and its metadata persist with the identity
  columns nulled out, which is deliberately privacy-conscious (a deleted
  account doesn't erase the fact that something happened) without
  requiring a retention decision to get that property.

**If a retention cutoff is ever actually decided**, implementing it needs
to reckon with the append-only triggers on purpose, not route around
them by accident: the correct shape is a `SECURITY DEFINER` pruning
function (mirroring how every other privileged write in this schema
works) that explicitly disables the `BEFORE DELETE` trigger for the
duration of its own transaction, prunes, and re-enables it — never a
blanket `DROP TRIGGER` left down, which would silently remove the
append-only guarantee for everything else too.

## Review: who, how often, and on what trigger

At the current team size (one admin), a rigid calendar review of a
near-empty log is theater more than security practice. The real policy
is **trigger-driven, with a floor**:

- **Immediate review, every time**, for these event types — they are
  rare enough that "immediate" costs nothing and each one is either a
  real incident or worth confirming isn't one:
  - `login_anomaly_detected` — confirm this was actually the account
    owner on a new device/network, not a credential in someone else's
    hands. The `step_up_required` field in its metadata tells you
    whether TOTP already forced a re-proof; if `false` (no TOTP
    enrolled), this is the one signal with **no** automatic backstop,
    so it deserves the closer look.
  - `account_recovery_initiated` — cross-check against the actual
    out-of-band (Teams/phone) confirmation the recovery flow assumes
    happened. This event existing with no matching memory of that
    conversation is the clearest single "something is wrong" signal
    this system can produce.
  - `totp_step_up_failed` / `totp_locked_out` clusters — a handful of
    fumbled codes is normal; a lockout on an account that isn't
    currently trying to do anything sensitive is not.
- **A periodic baseline pass — quarterly is a reasonable floor** — scan
  for patterns a single alarming event wouldn't surface on its own:
  repeated `login_failed`/`registration_failed` against the same email
  (probing), any `invite_refused` the admin doesn't remember causing,
  and a general skim for event-type frequencies that look different
  from the previous quarter. Adjust the cadence up if the user base
  grows past "one admin can eyeball it," or down if quarterly turns out
  to reliably find nothing — this number is a starting point, not a
  commitment.

## Practical queries for a review pass

Run these as the admin role (RLS restricts `SELECT` on
`auth.auth_audit_logs` to `current_setting('app.current_user_role') =
'admin'`, so they need an admin-context connection, not `app_service`'s
default identity-less one).

Event-type frequency, most recent quarter:

```sql
SELECT event_type, count(*)
  FROM auth.auth_audit_logs
 WHERE created_at > now() - interval '3 months'
 GROUP BY event_type
 ORDER BY count(*) DESC;
```

Anomalous logins not backstopped by a step-up (the one gap this system
doesn't close automatically):

```sql
SELECT created_at, actor_user_id, metadata
  FROM auth.auth_audit_logs
 WHERE event_type = 'login_anomaly_detected'
   AND (metadata->>'step_up_required')::boolean = false
 ORDER BY created_at DESC;
```

Repeated failures against one address (probing signature) — `email`
lives in `metadata` rather than a column specifically because an
unmatched address may not correspond to a real account:

```sql
SELECT metadata->>'email' AS email, count(*)
  FROM auth.auth_audit_logs
 WHERE event_type = 'login_failed'
   AND created_at > now() - interval '3 months'
 GROUP BY metadata->>'email'
 HAVING count(*) > 5
 ORDER BY count(*) DESC;
```

Every administrative act, actor and target both, for a specific
timeframe (useful after any real incident, not just routine review):

```sql
SELECT created_at, event_type, actor_user_id, target_user_id, metadata
  FROM auth.auth_audit_logs
 WHERE event_type IN ('invite_created', 'invite_refused',
                       'account_recovery_initiated', 'session_revoked')
 ORDER BY created_at DESC;
```

## Revisit this document when

- The admin population grows beyond "one person can plausibly review
  everything" — the trigger-driven cadence above assumes that.
- A retention cutoff is actually decided (see above for the correct
  implementation shape).
- The right-to-erasure/anonymize initiative is built, which will need to
  define what "anonymize" means for rows this table already handles
  gracefully via `ON DELETE SET NULL`.
