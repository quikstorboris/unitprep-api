-- Enrolled factors do not survive deactivation or soft-deletion.
--
-- Decision, 2026-07-30: when a user is deactivated or soft-deleted, their
-- passkeys (and TOTP secret, once that exists) are removed, so restoring
-- the account requires enrolling again. Boris's call, and the reasoning is
-- worth keeping: the UX cost is small and lands only on a genuinely
-- restored account, while the alternative means an offboarded person's
-- credential stays valid indefinitely, waiting to work again the moment
-- anyone flips their status back. A device can change hands between
-- offboarding and restoration; a key that outlives the offboarding is a
-- key nobody is tracking.
--
-- The account's HISTORY is untouched. Audit rows, and anything else
-- attributed to the user, survive -- restoring the account restores access
-- to prior work, which is an explicit product goal. What does not survive
-- is the means of authenticating. Those are separate concerns and this
-- separates them.
--
-- ## Why a trigger and not application code
--
-- Nothing deactivates a user yet -- there is no endpoint, and the admin UI
-- does not exist. Putting this in the eventual deactivation handler would
-- mean the guarantee holds only for the code paths that remember it, and
-- there will be several: an admin deactivating someone, an offboarding
-- routine, a break-glass action, a bulk operation, plus manual SQL during
-- an incident. A trigger holds for all of them, including the ones written
-- by someone who never read this file.
--
-- ## Why SECURITY DEFINER, which is the subtle part
--
-- webauthn_credentials and totp_credentials are guarded by owner-only RLS
-- keyed on app.current_user_id. An admin deactivating a DIFFERENT user has
-- their own id in that setting, so a SECURITY INVOKER trigger's DELETE
-- would match zero rows and delete nothing -- silently. The deactivation
-- would appear to succeed while leaving the credentials in place, which is
-- precisely the failure this migration exists to prevent, made invisible.
-- SECURITY DEFINER runs as the table owner, which is exempt from those
-- policies.
--
-- Fires only on the transition INTO a deactivated or deleted state, not on
-- every update of an already-deactivated row, so re-saving such a user is
-- not a repeated no-op delete.

CREATE FUNCTION auth.expire_factors_on_deactivation()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
BEGIN
    DELETE FROM auth.webauthn_credentials WHERE user_id = NEW.id;
    DELETE FROM auth.totp_credentials WHERE user_id = NEW.id;
    RETURN NULL;
END;
$$;

-- The WHEN clause carries the transition test rather than an IF inside the
-- body, so the function is not entered at all for unrelated updates.
CREATE TRIGGER users_expire_factors_on_deactivation
    AFTER UPDATE ON auth.users
    FOR EACH ROW
    WHEN (
        (NEW.status = 'deactivated' AND OLD.status IS DISTINCT FROM 'deactivated')
        OR (NEW.deleted_at IS NOT NULL AND OLD.deleted_at IS NULL)
    )
    EXECUTE FUNCTION auth.expire_factors_on_deactivation();

-- Ordinary callers must not be able to invoke this directly; it is only
-- meaningful as a trigger. Postgres runs trigger functions regardless of
-- EXECUTE privilege, so revoking is safe and closes it as a callable
-- SECURITY DEFINER entry point.
REVOKE EXECUTE ON FUNCTION auth.expire_factors_on_deactivation() FROM PUBLIC;
