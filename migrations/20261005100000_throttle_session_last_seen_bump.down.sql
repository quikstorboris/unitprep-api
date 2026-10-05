-- Restores the 20261002180000 definition: an unconditional last_seen_at
-- bump on every resolve.
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
        (SELECT array_agg(DISTINCT k ORDER BY k) FROM (
             SELECT rp.permission_key AS k
               FROM auth.user_roles ur
               JOIN auth.role_permissions rp ON rp.role_id = ur.role_id
              WHERE ur.user_id = u.id
             UNION
             SELECT up.permission_key
               FROM auth.user_permissions up
              WHERE up.user_id = u.id
         ) granted),
        s.elevated_until,
        s.requires_step_up,
        s.passkey_reverified_until;
$$;
