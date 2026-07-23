-- Reverses 20260723150000_move_auth_objects_to_auth_schema.up.sql.

ALTER TABLE auth.auth_audit_logs SET SCHEMA public;
ALTER TABLE auth.auth_configuration SET SCHEMA public;
ALTER TABLE auth.user_invites SET SCHEMA public;
ALTER TABLE auth.sessions SET SCHEMA public;
ALTER TABLE auth.totp_credentials SET SCHEMA public;
ALTER TABLE auth.webauthn_credentials SET SCHEMA public;
ALTER TABLE auth.users SET SCHEMA public;

ALTER FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) SET search_path = public;
ALTER FUNCTION auth.resolve_session(BYTEA) SET search_path = public;
ALTER FUNCTION auth.resolve_invite(BYTEA) SET search_path = public;
ALTER FUNCTION auth.consume_invite(BYTEA) SET search_path = public;

ALTER FUNCTION auth.create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT) SET SCHEMA public;
ALTER FUNCTION auth.resolve_session(BYTEA) SET SCHEMA public;
ALTER FUNCTION auth.resolve_invite(BYTEA) SET SCHEMA public;
ALTER FUNCTION auth.consume_invite(BYTEA) SET SCHEMA public;
ALTER FUNCTION auth.prevent_audit_log_mutation() SET SCHEMA public;
ALTER FUNCTION auth.set_updated_at() SET SCHEMA public;

ALTER TYPE auth.user_deletion_reason SET SCHEMA public;
ALTER TYPE auth.user_status SET SCHEMA public;
ALTER TYPE auth.user_company SET SCHEMA public;
ALTER TYPE auth.auth_role SET SCHEMA public;

DROP SCHEMA auth;
