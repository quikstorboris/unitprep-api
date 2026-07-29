-- Pre-authentication credential lookup for the WebAuthn login ceremony
-- (Phase 2 task 5).
--
-- Why this has to be SECURITY DEFINER rather than a plain query: at
-- POST /auth/login/begin there is no session yet, so no
-- app.current_user_id GUC has been set. webauthn_credentials is guarded
-- by an owner-only policy keyed on exactly that setting, so app_service
-- reading the table directly at that point sees zero rows for everyone --
-- which would make login structurally impossible, not merely awkward.
--
-- The mirror-image half of the ceremony does NOT need a function like
-- this: by POST /auth/login/finish the ceremony has a server-side
-- user_id that was never client-supplied, so the finish handler can set
-- the GUC to it and read/write the same rows through normal RLS. Only
-- the first leg, which starts from a client-supplied email and nothing
-- else, needs to bypass.
--
-- Returns one row per registered credential. A user with several
-- passkeys yields several rows with the same user_id; the caller
-- reassembles them into the credential set that
-- start_passkey_authentication needs.
--
-- ZERO ROWS is deliberately ambiguous, covering all of:
--   * no user with that email
--   * user exists but is not status = 'active'
--   * user exists but is soft-deleted
--   * user exists and is active but has no passkey registered
--
-- The caller must answer all four identically. Distinguishing them turns
-- an unauthenticated endpoint into a user-enumeration oracle, exactly as
-- resolve_bootstrap_registration avoids for the registration path.
--
-- Residual leak, accepted deliberately: an attacker can still infer
-- account existence from whether a *usable challenge* comes back at all,
-- because the identified (non-discoverable) WebAuthn flow has to name the
-- caller's credential ids in allowCredentials. Closing that would mean
-- fabricating plausible challenges for non-existent accounts, which
-- costs real complexity and hands a real user a passkey prompt that can
-- never succeed. Not worth it here: a passkey is phishing-resistant and
-- origin-bound, so knowing an address is enrolled buys an attacker
-- nothing on its own. Revisit if discoverable credentials
-- (start_discoverable_authentication) are adopted, which removes the
-- need to name credentials up front and closes this for free.
--
-- Schema-qualified throughout: the owner/migration connection's
-- search_path does not include `auth`, so unqualified names fail at
-- CREATE FUNCTION time for a LANGUAGE sql body.

CREATE FUNCTION auth.resolve_login_candidate(p_email citext)
RETURNS TABLE (user_id UUID, credential_id BYTEA, passkey_data JSONB)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT wc.user_id, wc.credential_id, wc.passkey_data
    FROM auth.users u
    JOIN auth.webauthn_credentials wc ON wc.user_id = u.id
    WHERE u.email = p_email
      AND u.status = 'active'
      AND u.deleted_at IS NULL;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_login_candidate(citext) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_login_candidate(citext) TO app_service;
