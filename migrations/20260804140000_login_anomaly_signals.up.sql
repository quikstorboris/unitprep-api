-- Phase II hardening, item 4: anomaly / risk-based auth signals. A login
-- from an IP address or user_agent never seen before for that account is
-- flagged -- audited unconditionally, and gated behind a fresh TOTP step-up
-- immediately after login when the account has TOTP confirmed (there is no
-- factor to step up with otherwise, so an unenrolled account is
-- audit-only -- forcing a lockout over a self-service factor nobody set up
-- would be a denial-of-service on that account, not a hardening measure).
--
-- "Unexpected location" is scoped down to "new IP address" rather than true
-- geolocation for now: real geography needs a GeoIP database (MaxMind
-- GeoLite2 or similar) as a new dependency, which is out of scope for this
-- pass and easy to layer on top of the ip_address column later without
-- another schema change.

-- auth.sessions.ip_address has existed since the original schema but was
-- never actually populated -- both call sites (auth_login.rs, auth_register.rs)
-- passed a literal NULL, since capturing a real client IP needed
-- into_make_service_with_connect_info wiring in main.rs that didn't exist
-- yet at the time. It does now (added for the auth rate limiter, see
-- api::router's GovernorLayer setup), so this migration's Rust-side
-- counterpart starts passing the real peer address through.
ALTER TABLE auth.sessions ADD COLUMN requires_step_up BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN auth.sessions.requires_step_up IS
    'Set at login when the account has TOTP confirmed and this login looked anomalous (new IP or new user_agent for this account, with at least one prior session to compare against). Cleared by auth.record_step_up once the caller proves a fresh TOTP code. While true, AuthenticatedUser refuses every route except the small set needed to clear it (step-up itself, sign-out, whoami) -- see src/auth/authenticated_user.rs.';

-- Adding p_requires_step_up is a signature change, not a body edit --
-- CREATE OR REPLACE cannot do this safely: Postgres resolves functions by
-- (schema, name, argument types), so "replacing" a 5-argument function with
-- a 6-argument one does not replace anything -- it silently creates a
-- SECOND, overloaded function, leaving the original 5-argument one in
-- place. Worse, the new overload would get Postgres's default privilege
-- (EXECUTE grants to PUBLIC unless revoked), quietly breaking the
-- revoke-from-PUBLIC/grant-to-app_service pattern every other function in
-- this schema follows. Drop and recreate, same as resolve_session below
-- and record_totp_success/resolve_session earlier today. Also rewritten
-- fully schema-qualified in the body (auth.users, auth.sessions) rather
-- than relying on the search_path pin, per the standing rule for any
-- migration written after the schema-to-auth move.
DROP FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT);

CREATE FUNCTION auth.create_session(
    p_user_id UUID,
    p_token_hash BYTEA,
    p_expires_at TIMESTAMPTZ,
    p_ip_address INET,
    p_user_agent TEXT,
    p_requires_step_up BOOLEAN
) RETURNS UUID
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    new_id UUID;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM auth.users
        WHERE id = p_user_id AND deleted_at IS NULL AND status = 'active'
    ) THEN
        RAISE EXCEPTION 'cannot create session for inactive or unknown user';
    END IF;

    INSERT INTO auth.sessions (user_id, token_hash, expires_at, ip_address, user_agent, requires_step_up)
    VALUES (p_user_id, p_token_hash, p_expires_at, p_ip_address, p_user_agent, p_requires_step_up)
    RETURNING id INTO new_id;

    RETURN new_id;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT, BOOLEAN) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT, BOOLEAN) TO app_service;

-- resolve_session's OUTPUT is changing again (requires_step_up added) --
-- drop and recreate, same reasoning as the idle-timeout migration earlier
-- today.
DROP FUNCTION auth.resolve_session(BYTEA, INTEGER);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (user_id UUID, role auth.auth_role, elevated_until TIMESTAMPTZ, requires_step_up BOOLEAN)
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
    RETURNING u.id, u.role, s.elevated_until, s.requires_step_up;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) TO app_service;

-- record_step_up now also clears requires_step_up -- a session that proved
-- a fresh TOTP code has satisfied the login-time gate regardless of
-- whether that was the reason it called this endpoint (an ordinary
-- sensitive-action step-up on a session that was never gated simply finds
-- requires_step_up already false, so this is a harmless no-op for that
-- case). Same signature and return type, so CREATE OR REPLACE is enough --
-- no drop needed.
CREATE OR REPLACE FUNCTION auth.record_step_up(p_token_hash BYTEA, p_minutes INTEGER)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    v_row_count INTEGER;
BEGIN
    UPDATE auth.sessions
       SET elevated_until = now() + make_interval(mins => p_minutes),
           requires_step_up = false
     WHERE token_hash = p_token_hash
       AND revoked_at IS NULL
       AND expires_at > now();

    GET DIAGNOSTICS v_row_count = ROW_COUNT;
    RETURN v_row_count > 0;
END;
$$;
