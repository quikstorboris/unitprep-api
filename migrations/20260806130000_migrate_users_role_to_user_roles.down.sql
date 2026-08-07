-- Reverses migrate_users_role_to_user_roles. Lossy for any user who ends
-- up holding more than one role by the time this ever runs: only ONE
-- role gets written back into the recreated single-value column,
-- preferring 'admin' if held, otherwise the earliest-granted role that
-- still fits the old enum, otherwise 'admin' by default. Documented here
-- rather than silently papered over -- there is no lossless way to
-- collapse a set back into a scalar.

CREATE TYPE auth.auth_role AS ENUM ('admin', 'onboarding_manager');

ALTER TABLE auth.users ADD COLUMN role auth.auth_role;

UPDATE auth.users u
SET role = COALESCE(
    (SELECT 'admin'::auth.auth_role
       FROM auth.user_roles ur JOIN auth.roles r ON r.id = ur.role_id
      WHERE ur.user_id = u.id AND r.key = 'admin'),
    (SELECT r.key::auth.auth_role
       FROM auth.user_roles ur JOIN auth.roles r ON r.id = ur.role_id
      WHERE ur.user_id = u.id AND r.key IN ('admin', 'onboarding_manager')
      ORDER BY ur.granted_at
      LIMIT 1),
    'admin'
);

ALTER TABLE auth.users ALTER COLUMN role SET NOT NULL;
ALTER TABLE auth.users ALTER COLUMN role SET DEFAULT 'admin';

-- Recreate set_user_role, as it was before this migration dropped it.
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

-- Restore set_user_status's original GUC-based admin check.
CREATE OR REPLACE FUNCTION auth.set_user_status(p_user_id UUID, p_status auth.user_status)
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

-- Restore list_users_for_admin's pre-migration shape (single role column).
DROP FUNCTION auth.list_users_for_admin();

CREATE FUNCTION auth.list_users_for_admin()
RETURNS TABLE (
    id UUID,
    email TEXT,
    first_name TEXT,
    last_name TEXT,
    company TEXT,
    job_title TEXT,
    role TEXT,
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
    IF NULLIF(current_setting('app.current_user_role', true), '') IS DISTINCT FROM 'admin' THEN
        RAISE EXCEPTION 'list_users_for_admin requires an admin caller';
    END IF;

    RETURN QUERY
    SELECT u.id,
           u.email::text,
           u.first_name,
           u.last_name,
           u.company::text,
           u.job_title,
           u.role::text,
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

-- Restore resolve_session's pre-migration shape (single role column).
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

-- Restore the original GUC-based policies.
ALTER POLICY users_select_own_or_admin ON auth.users
    USING (
        id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

ALTER POLICY users_update_own_or_admin ON auth.users
    USING (
        id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

ALTER POLICY users_insert_admin_only ON auth.users
    WITH CHECK (current_setting('app.current_user_role', true) = 'admin');

ALTER POLICY sessions_select_own_or_admin ON auth.sessions
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

ALTER POLICY sessions_update_own_or_admin ON auth.sessions
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

ALTER POLICY user_invites_admin_only ON auth.user_invites
    USING (current_setting('app.current_user_role', true) = 'admin')
    WITH CHECK (current_setting('app.current_user_role', true) = 'admin');

ALTER POLICY auth_configuration_admin_only ON auth.auth_configuration
    USING (current_setting('app.current_user_role', true) = 'admin')
    WITH CHECK (current_setting('app.current_user_role', true) = 'admin');

ALTER POLICY auth_audit_logs_select_admin_only ON auth.auth_audit_logs
    USING (current_setting('app.current_user_role', true) = 'admin');
