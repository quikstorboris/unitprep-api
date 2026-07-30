-- Deactivation must also retire a PENDING INVITE, not only enrolled factors.
--
-- 20260730110000 removed passkeys and TOTP secrets on deactivation or
-- soft-deletion. It missed the third way into an account: an outstanding
-- invitation. A user deactivated while still `invited` kept an invite row
-- with `used_at IS NULL`, which reads as live.
--
-- Not currently exploitable, and that is worth stating precisely rather
-- than overselling the fix: `auth.resolve_invite_registration` checks
-- `status = 'invited'` AND `deleted_at IS NULL`, so a deactivated or
-- soft-deleted account's token already resolves to nothing. Verified
-- against a real soft-deleted account before writing this.
--
-- It is fixed anyway, for the same reason the original trigger exists at
-- all. That migration's own argument was that a guarantee living in
-- application code "holds only for the code paths that remember it" -- and
-- the invite guard currently lives in exactly one SQL function. A second
-- lookup written later, by someone who does not know the status check is
-- load-bearing, silently reopens it. Retiring the invite means the token is
-- dead in the data, not merely dead in the one query that happens to ask
-- correctly.
--
-- Secondary benefit, which is why this surfaced: "count the live invites"
-- now returns the truth. A row that is unusable but looks outstanding is
-- the kind of thing that misleads an operator during an incident.
--
-- ## Retire rather than delete
--
-- `used_at = now()` matches how both existing issuing paths retire a
-- superseded invite (`bootstrap-admin --reissue-invite` and
-- `POST /auth/invites`). Deleting the row instead would erase the evidence
-- that an invitation was ever issued, which the audit trail should keep.
-- `used_at` is admittedly doing double duty for "redeemed" and "retired" --
-- the schema has no third state, and inventing one for this is not worth a
-- column when no caller distinguishes them.
--
-- ## Renamed, because "factors" was no longer honest
--
-- A pending invite is not an authentication factor; it is an authorization
-- to enrol one. What the trigger actually does is revoke every remaining
-- path into the account, so it now says that. Same precedent as
-- 20260730100000, which spent a migration making two foreign keys declare
-- what they really did.

-- SECURITY DEFINER for the same subtle reason as before: user_invites is
-- guarded by an admin-only policy and webauthn_credentials/totp_credentials
-- by owner-only policies keyed on app.current_user_id. An admin
-- deactivating a DIFFERENT user has their own id in that setting, so a
-- SECURITY INVOKER trigger would match zero rows and silently do nothing.
CREATE FUNCTION auth.revoke_access_paths_on_deactivation()
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

    RETURN NULL;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.revoke_access_paths_on_deactivation() FROM PUBLIC;

DROP TRIGGER users_expire_factors_on_deactivation ON auth.users;

-- Same WHEN clause: fires only on the transition INTO a deactivated or
-- deleted state, so re-saving an already-deactivated row is not a repeated
-- no-op.
CREATE TRIGGER users_revoke_access_paths_on_deactivation
    AFTER UPDATE ON auth.users
    FOR EACH ROW
    WHEN (
        (NEW.status = 'deactivated' AND OLD.status IS DISTINCT FROM 'deactivated')
        OR (NEW.deleted_at IS NOT NULL AND OLD.deleted_at IS NULL)
    )
    EXECUTE FUNCTION auth.revoke_access_paths_on_deactivation();

DROP FUNCTION auth.expire_factors_on_deactivation();

-- Retire invites belonging to accounts that were deactivated BEFORE this
-- trigger covered them. Without this the fix applies only to future
-- deactivations and leaves the existing misleading rows in place -- which
-- is exactly the state that prompted it.
UPDATE auth.user_invites ui
   SET used_at = now()
  FROM auth.users u
 WHERE u.id = ui.user_id
   AND ui.used_at IS NULL
   AND (u.status = 'deactivated' OR u.deleted_at IS NOT NULL);
