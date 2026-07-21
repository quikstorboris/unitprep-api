ALTER TABLE user_invites ENABLE ROW LEVEL SECURITY;

CREATE POLICY user_invites_admin_only ON user_invites
    FOR ALL
    USING (current_setting('app.current_user_role', true) = 'admin')
    WITH CHECK (current_setting('app.current_user_role', true) = 'admin');

ALTER TABLE user_invites ALTER COLUMN created_by
    SET DEFAULT NULLIF(current_setting('app.current_user_id', true), '')::uuid;

CREATE FUNCTION resolve_invite(p_token_hash BYTEA)
RETURNS TABLE (invite_id UUID, user_id UUID)
LANGUAGE sql
SECURITY DEFINER
SET search_path = public
AS $$
    SELECT ui.id, ui.user_id
    FROM user_invites ui
    WHERE ui.token_hash = p_token_hash
      AND ui.used_at IS NULL
      AND ui.expires_at > now();
$$;

CREATE FUNCTION consume_invite(p_token_hash BYTEA)
RETURNS UUID
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public
AS $$
DECLARE
    resolved_user_id UUID;
BEGIN
    UPDATE user_invites
    SET used_at = now()
    WHERE token_hash = p_token_hash
      AND used_at IS NULL
      AND expires_at > now()
    RETURNING user_id INTO resolved_user_id;

    IF resolved_user_id IS NOT NULL THEN
        UPDATE users SET status = 'active' WHERE id = resolved_user_id AND status = 'invited';
    END IF;

    RETURN resolved_user_id;
END;
$$;

REVOKE EXECUTE ON FUNCTION resolve_invite(BYTEA) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION consume_invite(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION resolve_invite(BYTEA) TO app_service;
GRANT EXECUTE ON FUNCTION consume_invite(BYTEA) TO app_service;
