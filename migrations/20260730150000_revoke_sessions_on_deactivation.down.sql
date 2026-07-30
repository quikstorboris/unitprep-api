-- Reverses 20260730150000, restoring the function to the body
-- 20260730130000 left it with -- credentials and invites, no sessions.
--
-- Sessions already revoked stay revoked. There is no un-revoke primitive
-- anywhere in this schema by design, and a schema rollback must not become
-- one.

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

    RETURN NULL;
END;
$$;
