-- Backs the new admin-facing "change this user's role" action (the
-- onboarding_manager role existed in the schema with no way to assign it
-- until now -- see auth_invites.rs's CreateInviteRequest for the other
-- half, role-at-invite-time). auth.users.role has no application-facing
-- UPDATE grant at all (see restrict_users_update_columns's REVOKE/GRANT
-- list -- role is explicitly named as "the escalation vector that
-- migration exists for"), so this is the only path an already-active
-- user's role can change through, mirroring auth.set_user_status exactly.
CREATE FUNCTION auth.set_user_role(p_user_id UUID, p_role auth.auth_role)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    v_row_count INTEGER;
BEGIN
    IF NULLIF(current_setting('app.current_user_role', true), '') IS DISTINCT FROM 'admin' THEN
        RAISE EXCEPTION 'set_user_role requires an admin caller';
    END IF;

    UPDATE auth.users
       SET role = p_role
     WHERE id = p_user_id
       AND deleted_at IS NULL;

    GET DIAGNOSTICS v_row_count = ROW_COUNT;
    RETURN v_row_count > 0;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.set_user_role(UUID, auth.auth_role) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.set_user_role(UUID, auth.auth_role) TO app_service;
