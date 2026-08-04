DROP FUNCTION auth.record_totp_success(UUID, BIGINT);

CREATE FUNCTION auth.record_totp_success(p_user_id UUID)
RETURNS VOID
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    UPDATE auth.totp_credentials
       SET failed_attempts = 0,
           locked_until = NULL,
           last_used_at = now()
     WHERE user_id = p_user_id;
$$;

REVOKE EXECUTE ON FUNCTION auth.record_totp_success(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.record_totp_success(UUID) TO app_service;

ALTER TABLE auth.totp_credentials DROP COLUMN last_used_step;

DROP FUNCTION auth.resolve_session(BYTEA, INTEGER);

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
