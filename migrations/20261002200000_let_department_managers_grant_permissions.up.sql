-- Department managers can see the Users page and grant individually-
-- grantable permissions (personal integrations such as ClickUp) -- and
-- nothing else on it. Boris's call, 2026-10-02: "restrict Dept mgrs in
-- their ability to invite, disable and changing roles."
--
-- Three things were admin-only and had to open up, each as narrowly as
-- possible:
--
-- 1. A new read permission, `users.view`, gates the user LISTING only.
--    It is deliberately not `users.manage` (invite, deactivate, recover,
--    reactivate, export) or `users.manage_roles`, which stay admin-only.
--    Admin holds both, so nothing changes for admins.
--
-- 2. `user_permissions.manage` (grant/revoke direct permissions) is also
--    given to department_manager. The permissions they can hand out are
--    still limited to the `directly_grantable` ones by a trigger, so this
--    cannot be used to grant anything privileged.
--
-- 3. Two SECURITY DEFINER functions did an admin-only check or relied on
--    an admin-only table read:
--    * `auth.list_users_for_admin()` now also admits department_manager.
--      (Its handler is still gated on `users.view`, and the CSV export on
--      `users.manage`, so a department manager can list but not export.)
--    * `auth.user_exists(uuid)` is new. The grant endpoints need to know
--      a target account exists, but `auth.users` is readable only by its
--      owner and admins under RLS, so a department manager's lookup
--      silently found nothing. Widening that table's SELECT policy would
--      expose far more than this needs; a one-bit existence function
--      exposes exactly what the check needs and nothing else.

INSERT INTO auth.permissions (key, label, description) VALUES
    ('users.view', 'View users', 'See the Users list (names, emails, roles, status, last activity). Does not allow inviting, disabling, recovering, or changing roles.');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT id, 'users.view' FROM auth.roles WHERE key IN ('admin', 'department_manager');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT id, 'user_permissions.manage' FROM auth.roles WHERE key = 'department_manager';

CREATE OR REPLACE FUNCTION auth.list_users_for_admin() RETURNS TABLE(id uuid, email text, first_name text, last_name text, company text, job_title text, role_keys text[], status text, created_at timestamp with time zone, credential_count bigint, totp_enrolled boolean, last_seen_at timestamp with time zone)
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO 'auth', 'public'
    AS $$
BEGIN
    IF NOT (auth.current_user_has_role('admin') OR auth.current_user_has_role('department_manager')) THEN
        RAISE EXCEPTION 'list_users_for_admin requires an admin or department manager caller';
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

CREATE FUNCTION auth.user_exists(p_user_id uuid) RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO 'auth', 'public'
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM auth.users WHERE id = p_user_id AND deleted_at IS NULL
    );
$$;

REVOKE EXECUTE ON FUNCTION auth.user_exists(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.user_exists(uuid) TO app_service;
