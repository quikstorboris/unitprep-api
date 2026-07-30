-- Reverses 20260730130000, restoring auth.expire_factors_on_deactivation
-- byte-for-byte as 20260730110000 created it, so a down/up cycle lands on
-- the same definition rather than a paraphrase.
--
-- Note what a revert cannot undo: invites already retired by the trigger, or
-- by this migration's backfill, stay retired. `used_at` is a timestamp, not
-- a flag, and there is no record of which rows were retired by whom -- so
-- guessing which to reopen would be inventing data. Reopening an invitation
-- is in any case something an administrator should do deliberately via
-- reissue, not something a schema rollback should do silently.

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

REVOKE EXECUTE ON FUNCTION auth.expire_factors_on_deactivation() FROM PUBLIC;

DROP TRIGGER users_revoke_access_paths_on_deactivation ON auth.users;

CREATE TRIGGER users_expire_factors_on_deactivation
    AFTER UPDATE ON auth.users
    FOR EACH ROW
    WHEN (
        (NEW.status = 'deactivated' AND OLD.status IS DISTINCT FROM 'deactivated')
        OR (NEW.deleted_at IS NOT NULL AND OLD.deleted_at IS NULL)
    )
    EXECUTE FUNCTION auth.expire_factors_on_deactivation();

DROP FUNCTION auth.revoke_access_paths_on_deactivation();
