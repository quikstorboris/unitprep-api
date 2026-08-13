-- Adds a passkey-based step-up, symmetric to TOTP's own
-- (elevated_until/record_step_up, see totp_step_up_not_login): TOTP
-- already step-up-gates replacing a passkey (add_passkey in
-- step_up_actions); this is what gates replacing TOTP -- self-service
-- TOTP re-enrolment now requires a fresh passkey assertion first, so a
-- hijacked session cannot silently swap in an attacker-controlled TOTP
-- secret and use it to pass the existing add_passkey step-up gate.
--
-- A separate column from elevated_until, not a reuse of it, because the
-- two answer different questions ("did this session just prove a TOTP
-- code" vs "did this session just prove a passkey assertion") and a
-- future action might reasonably require either, or both -- collapsing
-- them into one column would make it impossible to express that.
--
-- Same per-session (not per-user) placement and same reasoning as
-- elevated_until: verifying a passkey on this browser's session must not
-- silently elevate every other session the same user has open elsewhere.
ALTER TABLE auth.sessions ADD COLUMN passkey_reverified_until TIMESTAMPTZ;

-- resolve_session's OUTPUT columns are changing (passkey_reverified_until
-- added), and CREATE OR REPLACE cannot change a function's return type --
-- drop and recreate, same as the last two changes to this function.
DROP FUNCTION auth.resolve_session(BYTEA, INTEGER);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (
    user_id UUID,
    role_keys TEXT[],
    permission_keys TEXT[],
    elevated_until TIMESTAMPTZ,
    requires_step_up BOOLEAN,
    passkey_reverified_until TIMESTAMPTZ
)
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
    RETURNING
        u.id,
        (SELECT array_agg(DISTINCT r.key ORDER BY r.key)
           FROM auth.user_roles ur
           JOIN auth.roles r ON r.id = ur.role_id
          WHERE ur.user_id = u.id),
        (SELECT array_agg(DISTINCT rp.permission_key ORDER BY rp.permission_key)
           FROM auth.user_roles ur
           JOIN auth.role_permissions rp ON rp.role_id = ur.role_id
          WHERE ur.user_id = u.id),
        s.elevated_until,
        s.requires_step_up,
        s.passkey_reverified_until;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) TO app_service;

-- Elevates exactly the session that just proved a fresh passkey assertion
-- -- keyed by that session's own token_hash, same reasoning as
-- record_step_up: a caller only ever acts on the one session it holds the
-- opaque token for, never on a user_id directly.
CREATE FUNCTION auth.record_passkey_reverify(p_token_hash BYTEA, p_minutes INTEGER)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    v_row_count INTEGER;
BEGIN
    UPDATE auth.sessions
       SET passkey_reverified_until = now() + make_interval(mins => p_minutes)
     WHERE token_hash = p_token_hash
       AND revoked_at IS NULL
       AND expires_at > now();

    GET DIAGNOSTICS v_row_count = ROW_COUNT;
    RETURN v_row_count > 0;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.record_passkey_reverify(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.record_passkey_reverify(BYTEA, INTEGER) TO app_service;
