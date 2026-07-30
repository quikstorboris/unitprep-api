-- TOTP as a login fallback (Phase 2 task 9): brute-force protection, plus
-- the SECURITY DEFINER lookup an unauthenticated verification needs.
--
-- ## Why a lockout is not optional here, unlike for passkeys
--
-- A passkey assertion cannot be guessed. A six-digit TOTP code can: there
-- are a million of them, three are accepted at any moment (current step plus
-- one either side, see auth::totp), and an attacker who knows an email
-- address can submit as many as the server will take. Without a limit, a
-- credential worth 20 bits of entropy per attempt is exposed to unlimited
-- attempts, which is not a fallback factor, it is a delay.
--
-- ## Why locking TOTP is not a denial of service on the account
--
-- Any lockout invites the obvious abuse -- submit bad codes for someone
-- else's address and lock them out. That is defanged here by two properties
-- rather than by clever heuristics:
--
--   1. **The lock is time-bounded**, not sticky. It expires on its own.
--   2. **TOTP is a *fallback*.** Passkey sign-in is a separate path that
--      consults none of this, so a locked TOTP credential does not stop the
--      owner signing in the way they normally do. Locking the fallback is
--      an inconvenience; locking an account would be an outage.
--
-- That second property is the whole reason a lockout is affordable, and it
-- stops being true the moment anything makes TOTP the only way in. If TOTP
-- ever becomes a primary or mandatory factor, revisit this: the same lockout
-- would then be a genuine account-denial primitive.

ALTER TABLE auth.totp_credentials
    ADD COLUMN failed_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN locked_until TIMESTAMPTZ;

COMMENT ON COLUMN auth.totp_credentials.failed_attempts IS
    'Consecutive failed verifications. Reset to 0 on success. Never a reason to refuse on its own -- locked_until is the gate.';
COMMENT ON COLUMN auth.totp_credentials.locked_until IS
    'Set when failed_attempts crosses the threshold; verification is refused until it passes. Time-bounded on purpose: see the migration that added it.';

-- The unauthenticated lookup. Same shape and same reasoning as
-- auth.resolve_login_candidate: at this point there is no session, so no
-- app.current_user_id GUC, and totp_credentials is guarded by an owner-only
-- policy -- an ordinary read would return zero rows for everybody and make
-- fallback sign-in structurally impossible.
--
-- Every eligibility rule lives here rather than in the handler:
--
--   * the user must be active and not soft-deleted
--   * the credential must be CONFIRMED -- a half-finished enrolment must
--     never be usable to sign in, which is the entire purpose of
--     confirmed_at
--
-- Returns the lock state alongside the secret rather than refusing outright,
-- so the handler can record the attempt and answer with the same opaque
-- response either way. A lookup that returned nothing when locked would make
-- "this account is locked" observable to an attacker, which tells them their
-- guessing is having an effect.
CREATE FUNCTION auth.resolve_totp_candidate(p_email citext)
RETURNS TABLE (user_id UUID, secret_encrypted BYTEA, is_locked BOOLEAN)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT u.id,
           t.secret_encrypted,
           (t.locked_until IS NOT NULL AND t.locked_until > now())
      FROM auth.users u
      JOIN auth.totp_credentials t ON t.user_id = u.id
     WHERE u.email = p_email
       AND u.status = 'active'
       AND u.deleted_at IS NULL
       AND t.confirmed_at IS NOT NULL;
$$;

-- Records a failed verification and locks the credential once the threshold
-- is crossed. Returns the resulting attempt count, which the caller logs but
-- never returns to the client.
--
-- The threshold and window are literals rather than configuration. A knob
-- here would be one more thing to get wrong in a deployment, and there is no
-- evidence yet that any particular value needs tuning -- five attempts then
-- fifteen minutes is the conventional shape, and moving it is a migration
-- away if reality disagrees.
CREATE FUNCTION auth.record_totp_failure(p_user_id UUID)
RETURNS INTEGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    attempts INTEGER;
BEGIN
    UPDATE auth.totp_credentials
       SET failed_attempts = failed_attempts + 1,
           locked_until = CASE
               WHEN failed_attempts + 1 >= 5 THEN now() + interval '15 minutes'
               ELSE locked_until
           END
     WHERE user_id = p_user_id
    RETURNING failed_attempts INTO attempts;

    RETURN coalesce(attempts, 0);
END;
$$;

-- Clears the failure state and stamps last_used_at. Separate from the
-- verification itself because verification happens in the application (the
-- database cannot decrypt the secret -- deliberately, since the key is not
-- in the database).
CREATE FUNCTION auth.record_totp_success(p_user_id UUID)
RETURNS VOID
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    UPDATE auth.totp_credentials
       SET failed_attempts = 0,
           locked_until = NULL,
           last_used_at = now()
     WHERE user_id = p_user_id;
$$;

REVOKE EXECUTE ON FUNCTION auth.resolve_totp_candidate(citext) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION auth.record_totp_failure(UUID) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION auth.record_totp_success(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.resolve_totp_candidate(citext) TO app_service;
GRANT EXECUTE ON FUNCTION auth.record_totp_failure(UUID) TO app_service;
GRANT EXECUTE ON FUNCTION auth.record_totp_success(UUID) TO app_service;
