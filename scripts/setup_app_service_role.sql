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

-- client_ops is the first non-auth domain schema. Same USAGE-doesn't-
-- survive-a-schema-move reasoning as the auth block above, and guarded
-- the same way for the same fresh-branch ordering reason.
DO
$$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = 'client_ops') THEN
        EXECUTE 'GRANT USAGE ON SCHEMA client_ops TO app_service';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA client_ops TO app_service';
        EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner IN SCHEMA client_ops '
                'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_service';
    ELSE
        RAISE NOTICE 'schema "client_ops" not present yet -- skipping its grants. Re-run this file after `sqlx migrate run`.';
    END IF;
END
$$;

-- clients: the OO x Process Street client/facility data (companies,
-- facilities, Facility Policies, people, merchant accounts, contract
-- orders, PS task status) -- see migration
-- 20260828120000_create_process_street_client_tables for why this is
-- its own schema rather than folded into client_ops. Same
-- guarded-grant shape as every schema above.
DO
$$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = 'clients') THEN
        EXECUTE 'GRANT USAGE ON SCHEMA clients TO app_service';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA clients TO app_service';
        EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner IN SCHEMA clients '
                'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_service';
        -- Several tables here (policy_fees, policy_delinquency_steps,
        -- policy_coverage_tiers, facility_merchant_account_parties,
        -- ps_task_status) use BIGSERIAL primary keys. A table grant
        -- alone does not cover its underlying sequence -- Postgres
        -- treats a sequence as its own grantable object, and an INSERT
        -- that calls nextval() on it fails with "permission denied for
        -- sequence" without this, even though the table grant looks
        -- complete. Caught live via this migration's own integration
        -- test (clients::repository::integration_tests) before this
        -- schema had any real caller -- worth checking client_ops.
        -- vendor_format (also BIGSERIAL) for the same latent gap
        -- separately, since this script never granted sequence access
        -- anywhere before now.
        EXECUTE 'GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA clients TO app_service';
        EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner IN SCHEMA clients '
                'GRANT USAGE, SELECT ON SEQUENCES TO app_service';
    ELSE
        RAISE NOTICE 'schema "clients" not present yet -- skipping its grants. Re-run this file after `sqlx migrate run`.';
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

-- auth_audit_logs is append-only (migration
-- 20260729230000_revoke_audit_log_mutation_grants). INSERT and SELECT are
-- retained by the blanket grant above and are both wanted; only the
-- mutation grants come back off.
DO
$$
BEGIN
    IF EXISTS (
        SELECT FROM pg_catalog.pg_tables
        WHERE schemaname = 'auth' AND tablename = 'auth_audit_logs'
    ) THEN
        EXECUTE 'REVOKE UPDATE, DELETE ON auth.auth_audit_logs FROM app_service';
    END IF;
END
$$;

-- client_ops.audit_log is append-only too, same reasoning as
-- auth_audit_logs immediately above (migration
-- 20260808130000_widen_qms_tag_access_and_add_client_ops_audit_log).
DO
$$
BEGIN
    IF EXISTS (
        SELECT FROM pg_catalog.pg_tables
        WHERE schemaname = 'client_ops' AND tablename = 'audit_log'
    ) THEN
        EXECUTE 'REVOKE UPDATE, DELETE ON client_ops.audit_log FROM app_service';
    END IF;
END
$$;
