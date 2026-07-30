-- Reverses 20260730120000_invite_registration_lookup.
--
-- Recreates auth.resolve_bootstrap_registration byte-for-byte as
-- 20260729200000_bootstrap_registration_lookup created it, so a down/up cycle
-- lands on the same definition rather than a paraphrase of it. Note that
-- reverting the schema alone does NOT restore the bootstrap enrolment path:
-- that also needs the application code and the AUTH_BOOTSTRAP_ENABLED gate
-- removed in the same commit.

CREATE FUNCTION auth.resolve_bootstrap_registration(p_email citext)
RETURNS TABLE (user_id UUID, first_name TEXT, last_name TEXT)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT u.id, u.first_name, u.last_name
    FROM auth.users u
    WHERE u.email = p_email
      AND u.status = 'active'
      AND u.deleted_at IS NULL
      AND NOT EXISTS (
          SELECT 1 FROM auth.webauthn_credentials wc WHERE wc.user_id = u.id
      );
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_bootstrap_registration(citext) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_bootstrap_registration(citext) TO app_service;

DROP FUNCTION auth.resolve_invite_registration(BYTEA);
