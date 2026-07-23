-- Groups every auth-domain object into its own `auth` schema, leaving
-- `public` for sqlx's own _sqlx_migrations table and any future
-- non-auth domains. Existing grants on the tables themselves (relacl)
-- survive a schema move untouched -- only schema-level USAGE needs
-- granting separately, see scripts/setup_app_service_role.sql.

CREATE SCHEMA auth;

-- Enum types
ALTER TYPE auth_role SET SCHEMA auth;
ALTER TYPE user_company SET SCHEMA auth;
ALTER TYPE user_status SET SCHEMA auth;
ALTER TYPE user_deletion_reason SET SCHEMA auth;

-- Functions (moved before the tables that own their triggers; trigger
-- and policy definitions are bound by OID, not name, so this ordering
-- is for readability, not correctness)
ALTER FUNCTION set_updated_at() SET SCHEMA auth;
ALTER FUNCTION prevent_audit_log_mutation() SET SCHEMA auth;
ALTER FUNCTION create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) SET SCHEMA auth;
ALTER FUNCTION resolve_session(BYTEA) SET SCHEMA auth;
ALTER FUNCTION resolve_invite(BYTEA) SET SCHEMA auth;
ALTER FUNCTION consume_invite(BYTEA) SET SCHEMA auth;

-- The SECURITY DEFINER functions pin their own search_path so they
-- can't be tricked by a caller's search_path -- that pin has to follow
-- the tables into the new schema, or they stop finding users/sessions/
-- user_invites the moment the tables move below.
ALTER FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) SET search_path = auth, public;
ALTER FUNCTION auth.resolve_session(BYTEA) SET search_path = auth, public;
ALTER FUNCTION auth.resolve_invite(BYTEA) SET search_path = auth, public;
ALTER FUNCTION auth.consume_invite(BYTEA) SET search_path = auth, public;

-- Tables
ALTER TABLE users SET SCHEMA auth;
ALTER TABLE webauthn_credentials SET SCHEMA auth;
ALTER TABLE totp_credentials SET SCHEMA auth;
ALTER TABLE sessions SET SCHEMA auth;
ALTER TABLE user_invites SET SCHEMA auth;
ALTER TABLE auth_configuration SET SCHEMA auth;
ALTER TABLE auth_audit_logs SET SCHEMA auth;
