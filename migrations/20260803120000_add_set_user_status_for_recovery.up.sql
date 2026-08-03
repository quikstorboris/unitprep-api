-- A generic, admin-gated primitive for changing another user's status.
--
-- `status` has no column-level UPDATE grant for app_service at all (see
-- 20260729210000_restrict_users_update_columns.up.sql -- "a user must not
-- be able to reactivate themselves... everything else is an
-- administrative act and must go through a SECURITY DEFINER function
-- that checks the caller is an admin"). This is that function, and the
-- first one whose safety depends on checking the caller's ROLE rather
-- than being scoped by a token or the caller's own user_id the way every
-- earlier SECURITY DEFINER function here is -- there is no token or
-- self-scoping available for "an admin changes someone else's status",
-- so the GUC check inside the function body is the only thing standing
-- between this and an unconditional door through the column restriction
-- that motivated withholding the grant in the first place.
--
-- Deliberately generic rather than recovery-specific: the next admin
-- feature that needs to change a status (deactivating someone, say) gets
-- this primitive too, rather than each writing its own copy of the same
-- admin check and UPDATE.
--
-- No transition validation beyond "not soft-deleted" -- which statuses a
-- caller may legitimately move to from which is a decision left to the
-- calling code, the same way `revoke_session` trusts its caller about
-- which session to revoke. `deleted_at IS NULL` in the WHERE clause is a
-- no-op guard, not an error: a caller racing a soft-delete gets zero rows
-- affected rather than resurrecting a deleted account by accident.
--
-- Returns whether a row was actually updated, rather than VOID -- a
-- caller that needs to know (see auth_invites.rs's recovery path, which
-- must not proceed to insert a fresh invite if the status flip silently
-- no-opped) cannot tell from a bare `SELECT function()`'s own row count:
-- that always reports one row back regardless of what happened inside
-- the function body, since the SELECT succeeded either way.
CREATE FUNCTION auth.set_user_status(p_user_id UUID, p_status auth.user_status)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    v_row_count INTEGER;
BEGIN
    IF NULLIF(current_setting('app.current_user_role', true), '') IS DISTINCT FROM 'admin' THEN
        RAISE EXCEPTION 'set_user_status requires an admin caller';
    END IF;

    UPDATE auth.users
       SET status = p_status
     WHERE id = p_user_id
       AND deleted_at IS NULL;

    GET DIAGNOSTICS v_row_count = ROW_COUNT;
    RETURN v_row_count > 0;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.set_user_status(UUID, auth.user_status) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.set_user_status(UUID, auth.user_status) TO app_service;
