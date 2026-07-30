-- Deactivation must revoke SESSIONS too, which is the access path the
-- trigger was missing while being named after covering all of them.
--
-- 20260730130000 renamed this function to
-- `revoke_access_paths_on_deactivation` on the grounds that a pending invite
-- is an access path, not an authentication factor. That name was still not
-- honest: a live session is the most direct access path there is, and it was
-- left untouched. Found while verifying task 10, when a count of live
-- sessions turned up one belonging to an account deactivated hours earlier.
--
-- Not exploitable, and the reason is worth recording precisely rather than
-- claiming a bigger fix than this is. `auth.resolve_session` requires
-- `u.status = 'active' AND u.deleted_at IS NULL`, so a deactivated user's
-- token already resolves to nothing -- verified by reading the deployed
-- function definition, not assumed. What existed was a row that looked live
-- and was not.
--
-- Fixed for the third time on the same argument, which is starting to look
-- like the actual lesson: a guarantee enforced in exactly one query holds
-- only for callers who remember to ask that way. `resolve_session` checks
-- status today; a second session-resolving path written later by someone who
-- does not know that check is load-bearing would silently accept a
-- deactivated user's token. Revoking makes the session dead in the data.
--
-- It also means "how many people are signed in right now" answers correctly,
-- and that sign-out-everywhere and deactivation leave the same observable
-- state rather than two different kinds of "not signed in".
--
-- ## Why the UPDATE here rather than calling the task 10 functions
--
-- `auth.revoke_session` and `auth.revoke_all_sessions_for_token` are keyed on
-- a token hash, deliberately, so they can only act on the account whose live
-- token the caller holds -- that is what makes them safe to expose to the
-- application role. A trigger has the user id and no token, and it is
-- already running as the table owner, so it writes directly. Reusing the
-- token-keyed functions here would mean adding a user-id-keyed variant,
-- which is exactly the denial-of-service-shaped primitive those functions
-- were designed to avoid providing.

CREATE OR REPLACE FUNCTION auth.revoke_access_paths_on_deactivation()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
BEGIN
    DELETE FROM auth.webauthn_credentials WHERE user_id = NEW.id;
    DELETE FROM auth.totp_credentials WHERE user_id = NEW.id;

    UPDATE auth.user_invites
       SET used_at = now()
     WHERE user_id = NEW.id
       AND used_at IS NULL;

    -- Same one-way property as the task 10 functions: the literal now(),
    -- guarded by `revoked_at IS NULL`, so this can only ever move a session
    -- from live to revoked and never back, and never rewrites an existing
    -- revocation timestamp.
    UPDATE auth.sessions
       SET revoked_at = now()
     WHERE user_id = NEW.id
       AND revoked_at IS NULL;

    RETURN NULL;
END;
$$;

-- Sessions belonging to accounts deactivated before this covered them.
-- Without the backfill the fix applies only to future deactivations and
-- leaves the misleading rows that prompted it -- the same omission that
-- would have been easy to make in 20260730130000.
UPDATE auth.sessions s
   SET revoked_at = now()
  FROM auth.users u
 WHERE u.id = s.user_id
   AND s.revoked_at IS NULL
   AND (u.status = 'deactivated' OR u.deleted_at IS NOT NULL);
