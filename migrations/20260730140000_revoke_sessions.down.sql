-- Reverses 20260730140000.
--
-- Note what reverting does NOT do: sessions already revoked stay revoked.
-- These functions can only set `revoked_at`, never clear it, and that is the
-- point -- a schema rollback must not become the un-revoke primitive the
-- migration exists to avoid providing.

DROP FUNCTION auth.revoke_all_sessions_for_token(BYTEA);
DROP FUNCTION auth.revoke_session(BYTEA);
