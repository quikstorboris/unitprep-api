-- Repurposes TOTP from a login-fallback factor to a step-up verification
-- for sensitive in-session actions. See auth/totp.rs and auth_totp.rs for
-- the full reasoning -- short version: admin-driven account recovery
-- (2026-08-03) now covers "lost your only passkey", the gap TOTP-as-login
-- used to plug, and a static shared secret sitting next to a hardware
-- passkey as an equally-capable full login path was undercutting the
-- passkey-first security model. Repurposed, TOTP earns a role the system
-- didn't have at all: defense-in-depth against a hijacked session
-- attempting a high-blast-radius action.
--
-- `elevated_until` lives on the session row, not the user row, because
-- elevation is per-DEVICE: verifying a code on this browser's session
-- must not silently elevate every other session the same user has open
-- elsewhere. NULL (the default) means "never elevated" -- indistinguishable
-- from "elevated in the past, now expired", which is fine, since both mean
-- "not currently elevated" to every caller that reads this column.
ALTER TABLE auth.sessions ADD COLUMN elevated_until TIMESTAMPTZ;

-- resolve_session's OUTPUT columns are changing (elevated_until added), and
-- CREATE OR REPLACE cannot change a function's return type -- drop and
-- recreate is the correct move here, not an oversight.
DROP FUNCTION auth.resolve_session(BYTEA);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA)
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
      AND u.id = s.user_id
      AND u.deleted_at IS NULL
      AND u.status = 'active'
    RETURNING u.id, u.role, s.elevated_until;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA) TO app_service;

-- Elevates exactly the session that just proved a fresh TOTP code -- keyed
-- by that session's own token_hash, the same identifier resolve_session
-- itself takes, and for the same reason: a caller only ever acts on the
-- one session it holds the opaque token for for, never on a user_id
-- directly (that would elevate every device a user is signed in on from
-- a single step-up on one of them).
--
-- No admin check, unlike auth.set_user_status -- self-scoped by token_hash
-- the same way resolve_session is, not a caller-asserts-another-identity
-- operation.
--
-- Returns whether a row was actually updated, matching set_user_status's
-- reasoning: a bare `SELECT function()` always reports one row back
-- regardless of what happened inside, so the caller (the step-up handler)
-- needs this to tell "elevated" from "session vanished/expired between
-- verifying the code and recording it" and respond accordingly.
CREATE FUNCTION auth.record_step_up(p_token_hash BYTEA, p_minutes INTEGER)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    v_row_count INTEGER;
BEGIN
    UPDATE auth.sessions
       SET elevated_until = now() + make_interval(mins => p_minutes)
     WHERE token_hash = p_token_hash
       AND revoked_at IS NULL
       AND expires_at > now();

    GET DIAGNOSTICS v_row_count = ROW_COUNT;
    RETURN v_row_count > 0;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.record_step_up(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.record_step_up(BYTEA, INTEGER) TO app_service;
