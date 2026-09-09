-- The Process Street settings page (admin-only, see
-- 20260909140000_add_integrations_manage_permission) grows its first
-- secret: PROCESS_STREET_API_KEY, previously env-var-only (see
-- process_street::config's own module doc), now editable from the page
-- alongside the existing sync interval. Same shape as
-- client_ops.dropbox_configuration's secret columns -- ciphertext only,
-- encrypted under the shared INTEGRATION_SECRETS_ENCRYPTION_KEY (see
-- src/integrations/secrets.rs), nullable until an admin saves one.
ALTER TABLE client_ops.process_street_settings
    ADD COLUMN api_key_ciphertext BYTEA;

-- This table's SELECT policy is intentionally staying any-authenticated
-- (client_ops.sync's system-role background loop reads
-- sync_interval_hours through it -- see that migration's own comment).
-- The new secret column is protected at the application layer instead:
-- api::process_street_settings::get_settings now requires
-- integrations.manage before returning anything from this table at all
-- (previously it had no permission check), so a non-admin caller can no
-- longer reach this row through the HTTP API even though RLS itself
-- still allows the SELECT.
