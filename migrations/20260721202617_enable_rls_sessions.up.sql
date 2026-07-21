ALTER TABLE sessions ENABLE ROW LEVEL SECURITY;

CREATE POLICY sessions_select_own_or_admin ON sessions
    FOR SELECT
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

CREATE POLICY sessions_update_own_or_admin ON sessions
    FOR UPDATE
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

CREATE POLICY sessions_insert_blocked ON sessions
    FOR INSERT
    WITH CHECK (false);

CREATE FUNCTION create_session(
    p_user_id UUID,
    p_token_hash BYTEA,
    p_expires_at TIMESTAMPTZ,
    p_ip_address INET,
    p_user_agent TEXT
) RETURNS UUID
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public
AS $$
DECLARE
    new_id UUID;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM users
        WHERE id = p_user_id AND deleted_at IS NULL AND status = 'active'
    ) THEN
        RAISE EXCEPTION 'cannot create session for inactive or unknown user';
    END IF;

    INSERT INTO sessions (user_id, token_hash, expires_at, ip_address, user_agent)
    VALUES (p_user_id, p_token_hash, p_expires_at, p_ip_address, p_user_agent)
    RETURNING id INTO new_id;

    RETURN new_id;
END;
$$;

CREATE FUNCTION resolve_session(p_token_hash BYTEA)
RETURNS TABLE (user_id UUID, role auth_role)
LANGUAGE sql
SECURITY DEFINER
SET search_path = public
AS $$
    UPDATE sessions s
    SET last_seen_at = now()
    FROM users u
    WHERE s.token_hash = p_token_hash
      AND s.revoked_at IS NULL
      AND s.expires_at > now()
      AND u.id = s.user_id
      AND u.deleted_at IS NULL
      AND u.status = 'active'
    RETURNING u.id, u.role;
$$;

REVOKE EXECUTE ON FUNCTION create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION resolve_session(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) TO app_service;
GRANT EXECUTE ON FUNCTION resolve_session(BYTEA) TO app_service;
