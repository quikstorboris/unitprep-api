-- Bootstrap registration lookup, added for Phase 2 task 4 (WebAuthn
-- registration HTTP endpoints). Lets an unauthenticated caller look up
-- exactly one thing: whether a given email belongs to an ACTIVE user
-- with NO webauthn credential registered yet. This is what makes the
-- very first passkey registration (before any invite/login flow exists)
-- possible without ever letting an anonymous caller enumerate arbitrary
-- user rows or re-register over an existing passkey -- both the active
-- status check and the "zero existing credentials" check are baked into
-- the function itself, not left to the caller to enforce.
--
-- Gated further at the application layer behind AUTH_BOOTSTRAP_ENABLED
-- (see src/api/auth_register.rs) -- this function alone is already safe
-- to have granted, but the env-var gate keeps the whole bootstrap path
-- from even being reachable once real invite/login (tasks 5-8) exist.
--
-- Every reference below is explicitly schema-qualified (auth.users, not
-- users) rather than relying on search_path -- confirmed live that the
-- owner/migration connection's default search_path ("$user", public)
-- does not include auth, so an unqualified reference would fail this
-- function's own CREATE-time validation, unlike the pre-schema-move
-- functions elsewhere in this migration history.

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
