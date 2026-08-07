-- The cutover companion to add_roles_permissions_tables: rewires every
-- existing admin-check policy/function from the single-role GUC
-- (app.current_user_role) to the multi-role helper
-- (auth.current_user_has_role, backed by app.current_user_roles), widens
-- resolve_session/list_users_for_admin to return a set of role keys
-- instead of one, migrates every existing user's single role into
-- auth.user_roles, grants Boris's account its second role directly (the
-- one sanctioned way around the self-role-edit-absolute rule -- this
-- migration runs as the table owner, which bypasses RLS entirely), and
-- finally drops auth.users.role and auth.auth_role now that nothing
-- references them.
--
-- This is a single clean cutover, not a dual-write period: there is one
-- real user today, so there is no zero-downtime rollout to choreograph.
-- The running unitprep-api binary will not authenticate correctly again
-- until its Rust side is updated to match (setting
-- app.current_user_roles instead of app.current_user_role, reading
-- resolve_session's new role_keys column) -- expected, not a bug, and the
-- very next piece of work.

-- Rewire the five existing admin-gated policies to the new helper.
ALTER POLICY users_select_own_or_admin ON auth.users
    USING (
        id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR auth.current_user_has_role('admin')
    );

ALTER POLICY users_update_own_or_admin ON auth.users
    USING (
        id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR auth.current_user_has_role('admin')
    );

ALTER POLICY users_insert_admin_only ON auth.users
    WITH CHECK (auth.current_user_has_role('admin'));

ALTER POLICY sessions_select_own_or_admin ON auth.sessions
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR auth.current_user_has_role('admin')
    );

-- app_service holds no table-level UPDATE grant on auth.sessions at all
-- (revoked outright by revoke_sessions_update) so this policy is
-- currently unreachable in practice -- rewritten anyway so it stays
-- correct if a future non-owner role ever needs row-scoped session
-- visibility, per the RLS Implementation note's own reasoning for
-- leaving the policy in place.
ALTER POLICY sessions_update_own_or_admin ON auth.sessions
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR auth.current_user_has_role('admin')
    );

ALTER POLICY user_invites_admin_only ON auth.user_invites
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));

ALTER POLICY auth_configuration_admin_only ON auth.auth_configuration
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));

ALTER POLICY auth_audit_logs_select_admin_only ON auth.auth_audit_logs
    USING (auth.current_user_has_role('admin'));

-- resolve_session's role output becomes plural. Aggregated via a scalar
-- subquery so this stays one round trip, same as before -- token_hash is
-- unique, so at most one row is ever returned regardless.
DROP FUNCTION auth.resolve_session(BYTEA, INTEGER);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (user_id UUID, role_keys TEXT[], elevated_until TIMESTAMPTZ, requires_step_up BOOLEAN)
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
        (SELECT array_agg(r.key ORDER BY r.key)
           FROM auth.user_roles ur
           JOIN auth.roles r ON r.id = ur.role_id
          WHERE ur.user_id = u.id),
        s.elevated_until,
        s.requires_step_up;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) TO app_service;

-- list_users_for_admin: same role_keys widening, plus the admin check
-- moves to the new helper.
DROP FUNCTION auth.list_users_for_admin();

CREATE FUNCTION auth.list_users_for_admin()
RETURNS TABLE (
    id UUID,
    email TEXT,
    first_name TEXT,
    last_name TEXT,
    company TEXT,
    job_title TEXT,
    role_keys TEXT[],
    status TEXT,
    created_at TIMESTAMPTZ,
    credential_count BIGINT,
    totp_enrolled BOOLEAN,
    last_seen_at TIMESTAMPTZ
)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
BEGIN
    IF NOT auth.current_user_has_role('admin') THEN
        RAISE EXCEPTION 'list_users_for_admin requires an admin caller';
    END IF;

    RETURN QUERY
    SELECT u.id,
           u.email::text,
           u.first_name,
           u.last_name,
           u.company::text,
           u.job_title,
           (SELECT array_agg(r.key ORDER BY r.key)
              FROM auth.user_roles ur
              JOIN auth.roles r ON r.id = ur.role_id
             WHERE ur.user_id = u.id),
           u.status::text,
           u.created_at,
           (SELECT count(*) FROM auth.webauthn_credentials c WHERE c.user_id = u.id),
           EXISTS (
               SELECT 1 FROM auth.totp_credentials t
                WHERE t.user_id = u.id AND t.confirmed_at IS NOT NULL
           ),
           (SELECT max(s.last_seen_at) FROM auth.sessions s WHERE s.user_id = u.id)
      FROM auth.users u
     WHERE u.deleted_at IS NULL
     ORDER BY u.created_at;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.list_users_for_admin() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.list_users_for_admin() TO app_service;

-- set_user_status's admin check moves to the new helper. Signature and
-- return type are unchanged, so CREATE OR REPLACE is enough here -- no
-- drop needed.
CREATE OR REPLACE FUNCTION auth.set_user_status(p_user_id UUID, p_status auth.user_status)
RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    v_row_count INTEGER;
BEGIN
    IF NOT auth.current_user_has_role('admin') THEN
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

-- set_user_role is superseded by the grant/revoke-role functions coming
-- in the next piece of work (role-management endpoints). Nothing calls
-- this in the interim -- the Rust handler that used to call it is part of
-- that same next piece.
DROP FUNCTION auth.set_user_role(UUID, auth.auth_role);

-- Migrate every existing user's single role into the join table.
INSERT INTO auth.user_roles (user_id, role_id)
SELECT u.id, r.id
FROM auth.users u
JOIN auth.roles r ON r.key = u.role::text;

-- Boris's own dual-hat account: admin (migrated above) plus
-- onboarding_manager, granted directly here since RLS structurally
-- refuses anyone -- including admin -- from granting or revoking their
-- own roles through the normal app path. This is the one sanctioned
-- bootstrap route, same pattern as how the very first admin account ever
-- came to exist.
INSERT INTO auth.user_roles (user_id, role_id)
SELECT u.id, r.id
FROM auth.users u, auth.roles r
WHERE u.email = 'bmaksimov@quikstor.com'
  AND r.key = 'onboarding_manager'
ON CONFLICT DO NOTHING;

ALTER TABLE auth.users DROP COLUMN role;
DROP TYPE auth.auth_role;
