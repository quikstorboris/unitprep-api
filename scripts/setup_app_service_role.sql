-- Per-branch/per-environment setup. Not part of sqlx's tracked
-- migrations (deliberately -- CREATE ROLE is cluster-level, not scoped to
-- one database/schema the way table migrations are). Run manually
-- against each Neon branch that will serve real application traffic
-- (dev: done 2026-07-21; re-run 2026-07-23 for the auth schema grants;
-- prod: done 2026-07-29).
--
-- Neon roles are per-BRANCH state, not project-wide: a role created on
-- one branch does not exist on a sibling branch, and a branch taken from
-- a parent only inherits roles that existed at branch time. Verified
-- 2026-07-29 -- app_service existed on dev for eight days while prod had
-- no such role.
--
-- Creates a non-owner role for the running application to connect as, so
-- RLS policies actually apply (table owners bypass RLS by default). Run
-- as the owner role (e.g. via NEON_DEV_DATABASE_URL_DIRECT).
--
-- After running this, set the role's real password yourself:
--   \password app_service
-- then fill it into NEON_DEV_DATABASE_URL_APP (or the prod equivalent)
-- in .env.local. This script never sets a real password -- a role with
-- no password set cannot authenticate at all, which is intentional.
--
--
-- ORDER OF OPERATIONS ON A FRESH BRANCH -- run this file TWICE:
--
--   1. psql "$URL" -f scripts/setup_app_service_role.sql   <-- creates the role
--   2. sqlx migrate run --database-url "$URL"
--   3. psql "$URL" -f scripts/setup_app_service_role.sql   <-- applies the grants
--
-- Why twice, rather than once in either position:
--
--   * The role must exist BEFORE migrations run. The RLS migrations end
--     with GRANT EXECUTE ... TO app_service on the SECURITY DEFINER
--     bootstrap functions, which fails outright with
--     'role "app_service" does not exist'. (Hit for real on the prod
--     branch 2026-07-29, which failed partway through migration
--     20260721202617 and had to be resumed.)
--
--   * The grants can only be applied AFTER migrations run, because the
--     auth schema, its tables, and _sqlx_migrations do not exist until
--     then.
--
-- Those two constraints point in opposite directions, so no single
-- invocation can satisfy both on an empty branch. Rather than split this
-- into two files whose ordering could be got wrong independently, every
-- statement below is guarded so the whole file is safe to run at ANY
-- point and simply applies whatever is currently applicable. Run it,
-- migrate, run it again; the result converges either way. On an
-- already-migrated branch a single run does everything.

DO
$$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'app_service') THEN
        CREATE ROLE app_service WITH
            LOGIN
            NOSUPERUSER
            NOCREATEDB
            NOCREATEROLE
            NOREPLICATION
            NOBYPASSRLS
            CONNECTION LIMIT -1;
    END IF;
END
$$;

-- `public` always exists, so these need no existence guard. It holds
-- only _sqlx_migrations today (the auth objects moved out, see the
-- move_auth_objects_to_auth_schema migration), but the grant is kept so
-- a future non-auth domain added to `public` inherits the same access
-- rather than needing this file edited again.
GRANT USAGE ON SCHEMA public TO app_service;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app_service;
ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_service;

-- The auth tables live in their own schema. Table-level grants survive a
-- schema move on their own, but schema-level USAGE does not, so it is
-- granted here same as for public.
--
-- Guarded on the schema existing: on a fresh branch this file runs once
-- before migrations (to create the role), at which point `auth` is not
-- there yet and an unguarded GRANT would abort the script and skip
-- everything after it. GRANT has no IF EXISTS form, hence the DO block.
DO
$$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = 'auth') THEN
        EXECUTE 'GRANT USAGE ON SCHEMA auth TO app_service';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA auth TO app_service';
        EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner IN SCHEMA auth '
                'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_service';
    ELSE
        RAISE NOTICE 'schema "auth" not present yet -- skipping its grants. Re-run this file after `sqlx migrate run`.';
    END IF;
END
$$;

-- The app never needs sqlx's own migration-tracking table -- only
-- sqlx-cli does, connecting as the owner role. Guarded for the same
-- reason as the auth block: it does not exist until the first migration
-- has run.
DO
$$
BEGIN
    IF EXISTS (
        SELECT FROM pg_catalog.pg_tables
        WHERE schemaname = 'public' AND tablename = '_sqlx_migrations'
    ) THEN
        EXECUTE 'REVOKE ALL ON public._sqlx_migrations FROM app_service';
    END IF;
END
$$;

-- Re-assert the UPDATE restrictions on auth.users and auth.sessions.
--
-- REQUIRED, not belt-and-braces: the blanket
-- 'GRANT ... UPDATE ... ON ALL TABLES IN SCHEMA auth' above re-grants
-- TABLE-level UPDATE on both tables, which silently undoes migrations
-- 20260729210000_restrict_users_update_columns and
-- 20260729220000_revoke_sessions_update, re-opening the role-escalation
-- and session-un-revoke gaps they closed. Running this file would
-- otherwise be a security regression with no error and no output to
-- notice.
--
-- Those migrations remain the source of truth for WHY these grants look
-- the way they do; the blocks below only keep this script from
-- converging on the wrong state. If either changes, change it there and
-- mirror it here -- they must agree.
DO
$$
BEGIN
    IF EXISTS (
        SELECT FROM pg_catalog.pg_tables
        WHERE schemaname = 'auth' AND tablename = 'users'
    ) THEN
        EXECUTE 'REVOKE UPDATE ON auth.users FROM app_service';
        EXECUTE 'GRANT UPDATE (first_name, last_name, job_title) ON auth.users TO app_service';
    END IF;
END
$$;

-- No column grant is re-issued for sessions: there is no legitimate
-- application-level UPDATE on that table. create_session and
-- resolve_session are SECURITY DEFINER and run as the owner, so they are
-- unaffected.
DO
$$
BEGIN
    IF EXISTS (
        SELECT FROM pg_catalog.pg_tables
        WHERE schemaname = 'auth' AND tablename = 'sessions'
    ) THEN
        EXECUTE 'REVOKE UPDATE ON auth.sessions FROM app_service';
    END IF;
END
$$;
