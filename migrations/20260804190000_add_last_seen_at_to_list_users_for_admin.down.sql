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
