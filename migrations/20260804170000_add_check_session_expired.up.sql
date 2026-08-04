-- Backs the new `session_expired_access_attempt` audit event. resolve_session
-- already collapses "no such session", "revoked", and "expired" (idle or
-- absolute) into the same zero-rows result -- correct for the mandatory
-- extractor's own purposes (every one of those cases is equally
-- "unauthenticated" to a caller), but it means the extractor has no way to
-- tell an ordinary stale cookie apart from a session that specifically
-- timed out, which is the one case worth a permanent row: it is evidence a
-- real session existed and its owner (or someone holding their cookie)
-- kept trying to use it after the fact.
--
-- Deliberately excludes revoked sessions -- a revoked session being
-- retried is either an already-cleared "sign out everywhere" or a stale
-- browser tab, not a new signal, and revocation already has its own
-- SESSION_REVOKED event at the time it happened.
CREATE FUNCTION auth.check_session_expired(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (user_id UUID)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT s.user_id
      FROM auth.sessions s
     WHERE s.token_hash = p_token_hash
       AND s.revoked_at IS NULL
       AND (
           s.expires_at <= now()
           OR s.last_seen_at <= now() - make_interval(mins => p_idle_minutes)
       )
     LIMIT 1;
$$;

REVOKE EXECUTE ON FUNCTION auth.check_session_expired(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.check_session_expired(BYTEA, INTEGER) TO app_service;
