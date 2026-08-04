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
       SET elevated_until = now() + make_interval(mins => p_minutes)
     WHERE token_hash = p_token_hash
       AND revoked_at IS NULL
       AND expires_at > now();

    GET DIAGNOSTICS v_row_count = ROW_COUNT;
    RETURN v_row_count > 0;
END;
$$;

DROP FUNCTION auth.resolve_session(BYTEA, INTEGER);

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

DROP FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT, BOOLEAN);

CREATE FUNCTION auth.create_session(
    p_user_id UUID,
    p_token_hash BYTEA,
    p_expires_at TIMESTAMPTZ,
    p_ip_address INET,
    p_user_agent TEXT
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

    INSERT INTO auth.sessions (user_id, token_hash, expires_at, ip_address, user_agent)
    VALUES (p_user_id, p_token_hash, p_expires_at, p_ip_address, p_user_agent)
    RETURNING id INTO new_id;

    RETURN new_id;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) TO app_service;

ALTER TABLE auth.sessions DROP COLUMN requires_step_up;
