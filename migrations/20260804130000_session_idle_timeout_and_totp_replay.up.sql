-- Phase II hardening, item 2 (session/TOTP hardening): idle session expiry
-- on top of the existing absolute expiry, and a TOTP replay window. See
-- AUTHENTICATION.md's Phase II list and src/auth/totp.rs's module docs for
-- the fuller reasoning; this migration carries the two pieces that live in
-- the database.
--
-- ## Idle expiry, alongside the absolute expiry that already existed
--
-- `auth.sessions.expires_at` has always been an absolute ceiling, fixed at
-- login time (`SESSION_LIFETIME_HOURS`, default 12h) and never extended.
-- That alone is not enough: a session left open and unattended for the
-- rest of its 12-hour window stays fully valid the whole time. Idle expiry
-- adds a second, independent clock -- `last_seen_at`, which `resolve_session`
-- already stamps on every authenticated request -- so a session goes stale
-- after a period of no activity, well before its absolute ceiling, without
-- touching the ceiling itself.
--
-- `p_idle_minutes` is a parameter rather than a literal or a GUC, matching
-- how `SESSION_LIFETIME_HOURS` already works: the app reads its own env var
-- and passes the value in, so tuning it is a config change, not a
-- migration.
--
-- `resolve_session`'s OUTPUT is unchanged (still user_id, role,
-- elevated_until), but its argument list is, and CREATE OR REPLACE cannot
-- add a parameter to an existing function without leaving the old
-- signature callable alongside it -- drop and recreate is correct here, the
-- same reasoning as the elevated_until migration before this one.
DROP FUNCTION auth.resolve_session(BYTEA);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (user_id UUID, role auth.auth_role, elevated_until TIMESTAMPTZ)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    UPDATE auth.sessions s
    SET last_seen_at = now()
    FROM auth.users u
    WHERE s.token_hash = p_token_hash
      AND s.revoked_at IS NULL
      AND s.expires_at > now()
      AND s.last_seen_at > now() - make_interval(mins => p_idle_minutes)
      AND u.id = s.user_id
      AND u.deleted_at IS NULL
      AND u.status = 'active'
    RETURNING u.id, u.role, s.elevated_until;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) TO app_service;

-- ## TOTP replay window
--
-- `auth::totp::verify_code` accepts the current time step plus one either
-- side (a 90-second window, see auth/totp.rs). Nothing before this
-- migration stopped the *same* code from being submitted twice inside that
-- window and succeeding both times -- a code observed once (shoulder-surfed,
-- caught in a screen share, read from a compromised terminal) was fully
-- reusable until it naturally expired. `last_used_step` records the TOTP
-- step (not just the wall-clock time `last_used_at` already carried) that
-- was last accepted, so the application can refuse a code matching that
-- step or an earlier one -- see auth::totp::verify_code's rewritten
-- signature, which now returns the matched step instead of a bare bool so
-- the caller has something to persist here.
ALTER TABLE auth.totp_credentials ADD COLUMN last_used_step BIGINT;

COMMENT ON COLUMN auth.totp_credentials.last_used_step IS
    'TOTP time-step (unix_time / 30) last accepted for this credential. A submitted code matching this step or earlier is a replay and must be refused even if it is otherwise a valid code for the current window.';

-- Signature is changing (new p_step argument), so this is drop-and-recreate
-- for the same reason as resolve_session above, not an oversight.
DROP FUNCTION auth.record_totp_success(UUID);

CREATE FUNCTION auth.record_totp_success(p_user_id UUID, p_step BIGINT)
RETURNS VOID
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    UPDATE auth.totp_credentials
       SET failed_attempts = 0,
           locked_until = NULL,
           last_used_at = now(),
           last_used_step = p_step
     WHERE user_id = p_user_id;
$$;

REVOKE EXECUTE ON FUNCTION auth.record_totp_success(UUID, BIGINT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.record_totp_success(UUID, BIGINT) TO app_service;
