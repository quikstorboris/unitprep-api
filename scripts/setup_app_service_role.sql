-- One-time, per-branch/per-environment setup. Not part of sqlx's tracked
-- migrations (deliberately -- CREATE ROLE is cluster-level, not scoped to
-- one database/schema the way table migrations are). Run manually, once,
-- against each Neon branch that will serve real application traffic
-- (dev: done 2026-07-21; prod: before go-live).
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

GRANT USAGE ON SCHEMA public TO app_service;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app_service;
ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_service;

-- The app never needs sqlx's own migration-tracking table -- only
-- sqlx-cli does, connecting as the owner role.
REVOKE ALL ON _sqlx_migrations FROM app_service;
