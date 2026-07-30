-- Task 6, invite acceptance. Replaces the env-gated bootstrap enrolment
-- lookup with one keyed on an invite token, which is what allows
-- AUTH_BOOTSTRAP_ENABLED to be deleted rather than merely left unset.
--
-- Why a SECURITY DEFINER lookup at all: this runs for a caller with no
-- session, so no app.current_user_id GUC is set, and both auth.users and
-- auth.webauthn_credentials are RLS-guarded. An ordinary read would return
-- zero rows for everyone and make first-credential enrolment structurally
-- impossible. Same reasoning as auth.resolve_login_candidate.
--
-- Every eligibility rule lives HERE rather than in the handler, so an
-- anonymous caller can neither enumerate users nor enrol a competing
-- credential over an existing one regardless of what the application does:
--
--   * the invite must be unused and unexpired
--   * the user must still be 'invited' -- an account that has already
--     completed enrolment must not be able to walk this path a second time
--   * the user must have ZERO webauthn credentials
--
-- It deliberately does NOT consume the invite. Consumption happens only
-- after the credential verifies, in the same transaction as the credential
-- insert (see src/api/auth_register.rs). Keeping resolve and consume
-- separate is what makes a cancelled authenticator prompt a retry rather
-- than a lockout: the user stays 'invited' with a live invite, so both a
-- fresh attempt and `bootstrap-admin --reissue-invite` still work.
--
-- Fully schema-qualified, per the standing rule that no search_path is set
-- on the application connection. LANGUAGE sql rather than plpgsql so the
-- body is validated at CREATE time instead of first call.

CREATE FUNCTION auth.resolve_invite_registration(p_token_hash BYTEA)
RETURNS TABLE (user_id UUID, email TEXT, first_name TEXT, last_name TEXT)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT u.id, u.email::text, u.first_name, u.last_name
    FROM auth.user_invites ui
    JOIN auth.users u ON u.id = ui.user_id
    WHERE ui.token_hash = p_token_hash
      AND ui.used_at IS NULL
      AND ui.expires_at > now()
      AND u.status = 'invited'
      AND u.deleted_at IS NULL
      AND NOT EXISTS (
          SELECT 1 FROM auth.webauthn_credentials wc WHERE wc.user_id = u.id
      );
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_invite_registration(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_invite_registration(BYTEA) TO app_service;

-- The bootstrap lookup this replaces, dropped rather than left orphaned.
--
-- Leaving it would leave a callable SECURITY DEFINER function that matches
-- any active user holding no credentials by EMAIL ALONE. Its only remaining
-- guard was an application-side env var, and that env var is being deleted
-- in this same change -- so an unused function would be the most permissive
-- enrolment lookup in the schema with nothing above it.
DROP FUNCTION auth.resolve_bootstrap_registration(citext);
