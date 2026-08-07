-- Fixes a real bug, not a planned enhancement: authenticated_user.rs's
-- query_session has selected `permission_keys` from auth.resolve_session
-- since the roles/permissions authorization-core work landed, but no
-- migration ever actually added that column to the function -- caught
-- live ("column \"permission_keys\" does not exist") the first time a
-- real login exercised the authenticated path rather than the
-- no-cookie/401 path every prior test and smoke-check happened to use.
--
-- Same shape as the role_keys aggregation added in
-- migrate_users_role_to_user_roles: a second scalar subquery, still one
-- round trip, still at most one row since token_hash is unique.
DROP FUNCTION auth.resolve_session(BYTEA, INTEGER);

CREATE FUNCTION auth.resolve_session(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (
    user_id UUID,
    role_keys TEXT[],
    permission_keys TEXT[],
    elevated_until TIMESTAMPTZ,
    requires_step_up BOOLEAN
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
        s.requires_step_up;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_session(BYTEA, INTEGER) TO app_service;
