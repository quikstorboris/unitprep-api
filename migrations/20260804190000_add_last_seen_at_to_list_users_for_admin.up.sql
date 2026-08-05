-- Backs the admin Users table's dormant-account indicator: the last time
-- any session for this user was active, or NULL if they have never had
-- one (e.g. still `invited`). Aggregated from auth.sessions rather than
-- added as a column on auth.users -- a user has zero or many sessions
-- over time, and the table already tracks per-session activity via
-- last_seen_at; duplicating that onto users would be a second place for
-- the same fact to drift out of sync.
--
-- Signature is unchanged (still zero arguments), but the RETURNS TABLE
-- shape is gaining a column -- CREATE OR REPLACE cannot do that for a
-- function whose body is not LANGUAGE sql with an unchanged return type
-- in every Postgres version's documented sense of "compatible", so this
-- follows the same drop-and-recreate precedent as resolve_session's own
-- migration.
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
