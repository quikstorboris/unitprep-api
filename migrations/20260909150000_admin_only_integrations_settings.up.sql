-- Companion to 20260909140000_add_integrations_manage_permission:
-- Process Street's settings write moves from client_ops.perform
-- (onboarding_manager/department_manager) to integrations.manage
-- (admin only), so its RLS policy -- the real enforcement layer,
-- app-layer `require_permission` is UX -- has to move with it or an
-- admin's app-layer-approved update would still be silently rejected at
-- the database.
DROP POLICY IF EXISTS process_street_settings_update_client_ops_roles
    ON client_ops.process_street_settings;

CREATE POLICY process_street_settings_update_admin_only
    ON client_ops.process_street_settings FOR UPDATE
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));

-- Dropbox integration settings -- the app-wide credentials
-- `dropbox::DropboxConfig` used to load exclusively from `DROPBOX_*` env
-- vars (see that module's own doc comment). Singleton row, same shape as
-- client_ops.process_street_settings: a SMALLINT primary key CHECKed to
-- 1, seeded here so the app never has to handle a missing row.
--
-- app_secret and refresh_token are the two genuinely sensitive values --
-- stored only as ChaCha20-Poly1305 ciphertext (`dropbox::config`'s own
-- encrypt_secret/decrypt_secret, under their own DROPBOX_CONFIG_
-- ENCRYPTION_KEY, deliberately not CLIENT_PII_ENCRYPTION_KEY or
-- TOTP_ENCRYPTION_KEY -- see clients::encryption's own doc comment on
-- why each credential class gets its own key). app_key/root_namespace_id/
-- root_path are stored as plain text: identifying values, not secrets on
-- their own, same tier as e.g. a client id.
--
-- All columns nullable (other than id): the row exists from the moment
-- this migration runs, but stays "not configured" (every column NULL)
-- until an admin fills it in via the settings page for the first time,
-- during which `dropbox::DropboxConfig::from_db` reports `None` and the
-- app falls back to `DROPBOX_*` env vars exactly like it did before this
-- table existed.
--
-- Admin-only read AND write, unlike process_street_settings' any-
-- authenticated read: that table's one column is an operational sync
-- interval, this one holds integration secrets, so even the read side
-- follows auth.auth_configuration's admin-only pattern rather than
-- process_street_settings' own.
CREATE TABLE client_ops.dropbox_configuration (
    id SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    app_key TEXT,
    app_secret_ciphertext BYTEA,
    refresh_token_ciphertext BYTEA,
    root_namespace_id TEXT,
    root_path TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES auth.users(id) ON DELETE SET NULL
);

CREATE TRIGGER dropbox_configuration_set_updated_at
    BEFORE UPDATE ON client_ops.dropbox_configuration
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

INSERT INTO client_ops.dropbox_configuration (id) VALUES (1);

ALTER TABLE client_ops.dropbox_configuration ENABLE ROW LEVEL SECURITY;

CREATE POLICY dropbox_configuration_admin_only
    ON client_ops.dropbox_configuration FOR SELECT
    USING (auth.current_user_has_role('admin'));

CREATE POLICY dropbox_configuration_update_admin_only
    ON client_ops.dropbox_configuration FOR UPDATE
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));
