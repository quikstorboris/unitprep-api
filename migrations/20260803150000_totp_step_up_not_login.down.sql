DROP FUNCTION auth.record_step_up(BYTEA, INTEGER);

DROP FUNCTION auth.resolve_session(BYTEA);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA)
RETURNS TABLE (user_id UUID, role auth.auth_role)
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
    RETURNING u.id, u.role;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA) TO app_service;

ALTER TABLE auth.sessions DROP COLUMN elevated_until;
