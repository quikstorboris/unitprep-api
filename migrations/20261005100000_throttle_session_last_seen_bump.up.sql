-- Efficiency Refactor A1: stop writing auth.sessions on every authenticated
-- request.
--
-- The previous definition (20261002180000) was one
-- `UPDATE auth.sessions SET last_seen_at = now() ... RETURNING`, which
-- meant every authenticated request took a row lock on its session,
-- wrote WAL and left a dead tuple -- and concurrent requests from one
-- browser tab (the SPA fires several in parallel) serialized on that
-- lock.
--
-- Now the lookup and the bump are separate CTEs. `live` is a plain read
-- carrying every validity check the UPDATE used to carry (token match,
-- not revoked, absolute expiry, idle window, user active and not
-- deleted), so what counts as a valid session is unchanged. `bump` only
-- writes when the stored last_seen_at is older than a throttle interval:
-- LEAST(60 s, 6 s per idle minute), so the default 30-minute idle timeout
-- gets a 60 s throttle and a tiny test-sized idle window still bumps
-- often enough to behave. (A data-modifying CTE always executes even
-- though nothing selects from it.)
--
-- Idle-timeout semantics: the stored last_seen_at can now lag the real
-- last activity by at most the throttle interval, so a session can
-- expire up to that interval EARLY -- never late. The effective idle
-- window is [idle - throttle, idle]. The absolute expires_at ceiling,
-- revocation, deactivation and every returned column are unaffected.
--
-- Signature and return shape are identical to 20261002180000, so the
-- Rust side (query_session) and the app_service EXECUTE grant (kept by
-- CREATE OR REPLACE) are unchanged.
CREATE OR REPLACE FUNCTION auth.resolve_session(p_token_hash bytea, p_idle_minutes integer)
RETURNS TABLE(user_id uuid, role_keys text[], permission_keys text[], elevated_until timestamp with time zone, requires_step_up boolean, passkey_reverified_until timestamp with time zone)
    LANGUAGE sql SECURITY DEFINER
    SET search_path TO 'auth', 'public'
    AS $$
    WITH live AS (
        SELECT s.token_hash,
               s.last_seen_at,
               s.elevated_until,
               s.requires_step_up,
               s.passkey_reverified_until,
               u.id AS uid
          FROM auth.sessions s
          JOIN auth.users u ON u.id = s.user_id
         WHERE s.token_hash = p_token_hash
           AND s.revoked_at IS NULL
           AND s.expires_at > now()
           AND s.last_seen_at > now() - make_interval(mins => p_idle_minutes)
           AND u.deleted_at IS NULL
           AND u.status = 'active'
    ),
    bump AS (
        UPDATE auth.sessions s
           SET last_seen_at = now()
          FROM live
         WHERE s.token_hash = live.token_hash
           AND s.revoked_at IS NULL
           AND s.last_seen_at < now() - make_interval(secs => LEAST(60, p_idle_minutes * 6))
        RETURNING 1
    )
    SELECT live.uid,
           (SELECT array_agg(DISTINCT r.key ORDER BY r.key)
              FROM auth.user_roles ur
              JOIN auth.roles r ON r.id = ur.role_id
             WHERE ur.user_id = live.uid),
           (SELECT array_agg(DISTINCT k ORDER BY k) FROM (
                SELECT rp.permission_key AS k
                  FROM auth.user_roles ur
                  JOIN auth.role_permissions rp ON rp.role_id = ur.role_id
                 WHERE ur.user_id = live.uid
                UNION
                SELECT up.permission_key
                  FROM auth.user_permissions up
                 WHERE up.user_id = live.uid
            ) granted),
           live.elevated_until,
           live.requires_step_up,
           live.passkey_reverified_until
      FROM live;
$$;
