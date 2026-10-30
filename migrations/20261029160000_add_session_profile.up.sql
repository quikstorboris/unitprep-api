-- Frontend Performance: take `GET /health/whoami` from six sequential
-- database statements to two.
--
-- `whoami` is the first call every page load makes and the whole UI waits
-- on it. After the mandatory extractor's `auth.resolve_session` (one
-- statement) it used to open an RLS transaction (BEGIN, `set_config`, two
-- reads, COMMIT = five more), and against Neon each statement is a network
-- round trip of tens of milliseconds, so the page sat blank for 200-550 ms.
--
-- This function returns exactly what those two reads returned -- the
-- caller's own first and last name and whether a CONFIRMED TOTP credential
-- exists -- in one statement.
--
-- Security shape, deliberately the same as `resolve_session` and
-- `check_session_expired`:
--   * Keyed by the session token hash, with NO user-id parameter: the only
--     user it can ever describe is the owner of a live session whose token
--     the caller already holds. It cannot be used to look up anyone else.
--   * Session validity is not re-implemented here. It calls
--     `auth.resolve_session`, so token match, not revoked, absolute expiry,
--     idle window and user active/not deleted are decided in exactly one
--     place and this function can never drift from it. A session that is no
--     longer valid yields zero rows. (The throttled `last_seen_at` bump
--     inside `resolve_session` is a no-op here: the extractor bumped it a
--     moment earlier, well inside the throttle interval.)
--   * Returns names and a boolean only. No secret, no email, no TOTP
--     secret, no credential id.
--   * SECURITY DEFINER with a pinned search_path and every object
--     schema-qualified; EXECUTE revoked from PUBLIC and granted to
--     `app_service` only.
--
-- Trade-off to know about: this read bypasses the row-level-security
-- policies that the old transaction applied, and relies on the token
-- scoping above instead. That is the same trust model every other
-- session-resolution function already uses; it is recorded in
-- THREAT_MODEL.md next to them.
--
-- Additive only: a new function, nothing altered. Deploy order is safe
-- both ways (migration first, then code; or roll the code back and leave
-- the function in place).
CREATE FUNCTION auth.session_profile(p_token_hash BYTEA, p_idle_minutes INTEGER)
RETURNS TABLE (first_name TEXT, last_name TEXT, totp_enrolled BOOLEAN)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT u.first_name,
           u.last_name,
           EXISTS (
               SELECT 1
                 FROM auth.totp_credentials t
                WHERE t.user_id = rs.user_id
                  AND t.confirmed_at IS NOT NULL
           )
      FROM auth.resolve_session(p_token_hash, p_idle_minutes) rs
      JOIN auth.users u ON u.id = rs.user_id;
$$;

REVOKE EXECUTE ON FUNCTION auth.session_profile(BYTEA, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.session_profile(BYTEA, INTEGER) TO app_service;
