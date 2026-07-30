-- Sign-out (Phase 2 task 10). Two SECURITY DEFINER functions, because
-- `app_service` has no UPDATE on auth.sessions at all and must not get one.
--
-- ## Why not simply re-grant UPDATE (revoked_at)
--
-- Because a column-level grant permits writing **NULL** exactly as readily
-- as a timestamp. `UPDATE auth.sessions SET revoked_at = NULL` would then be
-- available to the application role, letting a caller undo "sign out
-- everywhere" -- and letting them extend a session by clearing its
-- revocation. That hole was closed on 2026-07-29 by revoking UPDATE
-- outright; re-granting even one column reopens it. The whole reason an
-- opaque session token was chosen over a JWT is that revocation is real, so
-- an un-revoke primitive would defeat the central design decision.
--
-- These functions are therefore the *only* way revoked_at is ever written,
-- and they can only ever move it from NULL to now():
--
--   * the SET clause is the literal `now()`, so no caller-supplied value
--     reaches the column
--   * `WHERE revoked_at IS NULL` means an already-revoked row is not
--     touched, so a replayed sign-out cannot even shift the timestamp to
--     hide when the real one happened
--
-- ## Why both functions take a token hash rather than a user id
--
-- A function like `revoke_all_sessions(p_user_id)` would be a
-- denial-of-service primitive: anything holding EXECUTE could sign any user
-- out of everything, and correctness would rest on every present and future
-- handler passing the right id. Taking the token hash makes the functions
-- **self-authorizing** -- they derive the user from the presented session,
-- so they can only ever act on the account whose live token the caller
-- actually holds. Aiming one at someone else is not a bug you can write.
--
-- An admin-facing "sign this user out everywhere" will need the user-id
-- form, and it belongs with the admin panel: a separate function that
-- checks the caller is an admin, per the standing pattern for
-- administrative user changes. Deliberately not built ahead of that need.
--
-- Note also that `sessions_update_own_or_admin` remains as an RLS policy but
-- is inert for `app_service`, which holds no table-level UPDATE privilege --
-- privileges and policies are checked together, and the privilege is the
-- binding constraint. It is left in place rather than dropped so that any
-- future role granted UPDATE is still row-scoped rather than unrestricted.

-- Revokes exactly the session the caller presented. Returns the owning user
-- and 1, or (NULL, 0) when the token matches no unrevoked session -- which
-- is the ordinary idempotent case, not an error: signing out twice, or with
-- a stale cookie, must succeed quietly.
--
-- Deliberately does NOT require the session to be unexpired. An expired
-- session is already unusable, and refusing to mark it revoked would leave
-- rows that a cleanup sweep then has to reason about differently depending
-- on how the user happened to stop using them.
CREATE FUNCTION auth.revoke_session(p_token_hash BYTEA)
RETURNS TABLE (user_id UUID, revoked_count INTEGER)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    resolved UUID;
BEGIN
    UPDATE auth.sessions s
       SET revoked_at = now()
     WHERE s.token_hash = p_token_hash
       AND s.revoked_at IS NULL
    RETURNING s.user_id INTO resolved;

    IF resolved IS NULL THEN
        RETURN QUERY SELECT NULL::UUID, 0;
    ELSE
        RETURN QUERY SELECT resolved, 1;
    END IF;
END;
$$;

-- "Sign out everywhere". Resolves the presented token to its owner and
-- revokes every live session that owner has, including the one used to make
-- the request.
--
-- Requires the presented session to be **currently valid** (unrevoked and
-- unexpired), unlike revoke_session above. The asymmetry is deliberate:
-- signing yourself out of one session is harmless if the token is already
-- dead, but a dead token must not be able to sign a user out of every
-- device they have -- that would turn a leaked, expired cookie from
-- worthless into a usable nuisance.
CREATE FUNCTION auth.revoke_all_sessions_for_token(p_token_hash BYTEA)
RETURNS TABLE (user_id UUID, revoked_count INTEGER)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = auth, public
AS $$
DECLARE
    resolved UUID;
    affected INTEGER;
BEGIN
    SELECT s.user_id INTO resolved
      FROM auth.sessions s
     WHERE s.token_hash = p_token_hash
       AND s.revoked_at IS NULL
       AND s.expires_at > now();

    IF resolved IS NULL THEN
        RETURN QUERY SELECT NULL::UUID, 0;
        RETURN;
    END IF;

    WITH revoked AS (
        UPDATE auth.sessions s
           SET revoked_at = now()
         WHERE s.user_id = resolved
           AND s.revoked_at IS NULL
        RETURNING 1
    )
    SELECT count(*)::INTEGER INTO affected FROM revoked;

    RETURN QUERY SELECT resolved, affected;
END;
$$;

REVOKE EXECUTE ON FUNCTION auth.revoke_session(BYTEA) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION auth.revoke_all_sessions_for_token(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.revoke_session(BYTEA) TO app_service;
GRANT EXECUTE ON FUNCTION auth.revoke_all_sessions_for_token(BYTEA) TO app_service;
