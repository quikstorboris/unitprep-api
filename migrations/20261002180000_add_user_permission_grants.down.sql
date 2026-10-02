CREATE OR REPLACE FUNCTION auth.resolve_session(p_token_hash bytea, p_idle_minutes integer)
RETURNS TABLE(user_id uuid, role_keys text[], permission_keys text[], elevated_until timestamp with time zone, requires_step_up boolean, passkey_reverified_until timestamp with time zone)
    LANGUAGE sql SECURITY DEFINER
    SET search_path TO 'auth', 'public'
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
        s.requires_step_up,
        s.passkey_reverified_until;
$$;

DROP TABLE auth.user_permissions;
DROP FUNCTION auth.enforce_directly_grantable();
DELETE FROM auth.role_permissions WHERE permission_key IN ('integrations.clickup', 'user_permissions.manage');
DELETE FROM auth.permissions WHERE key IN ('integrations.clickup', 'user_permissions.manage');
ALTER TABLE auth.permissions DROP COLUMN category, DROP COLUMN directly_grantable;
