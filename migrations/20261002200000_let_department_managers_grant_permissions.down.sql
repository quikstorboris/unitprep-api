DROP FUNCTION auth.user_exists(uuid);

CREATE OR REPLACE FUNCTION auth.list_users_for_admin() RETURNS TABLE(id uuid, email text, first_name text, last_name text, company text, job_title text, role_keys text[], status text, created_at timestamp with time zone, credential_count bigint, totp_enrolled boolean, last_seen_at timestamp with time zone)
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO 'auth', 'public'
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

DELETE FROM auth.role_permissions
 WHERE permission_key = 'users.view'
    OR (permission_key = 'user_permissions.manage'
        AND role_id = (SELECT id FROM auth.roles WHERE key = 'department_manager'));
DELETE FROM auth.permissions WHERE key = 'users.view';
