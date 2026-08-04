-- Backs the admin Users tab (Phase I item 8). A plain SELECT under
-- begin_rls_transaction cannot do this join itself: auth.webauthn_credentials'
-- RLS policy has no admin-bypass clause (unlike auth.users/auth.totp_credentials),
-- so an admin-scoped query sees only their OWN credential rows, making
-- every other user's credential_count silently read as zero. A
-- SECURITY DEFINER function, checking the caller's role explicitly the
-- same way auth.set_user_status does, is the correct way past that --
-- not widening the RLS policy itself, which exists so an ordinary
-- self-service credential read never accidentally becomes admin-browsable.
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
    totp_enrolled BOOLEAN
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
           )
      FROM auth.users u
     WHERE u.deleted_at IS NULL
     ORDER BY u.created_at;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.list_users_for_admin() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.list_users_for_admin() TO app_service;
