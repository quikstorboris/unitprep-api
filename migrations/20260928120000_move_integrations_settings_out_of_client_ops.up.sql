-- Moves dropbox_configuration and process_street_settings out of
-- client_ops into their own integrations schema. Both tables have
-- been gated by the integrations.manage permission (not
-- client_ops.perform) since 20260909140000_add_integrations_manage_permission
-- / 20260909150000_admin_only_integrations_settings, but their schema
-- namespace never followed. client_ops (see src/client_ops/mod.rs's
-- own doc comment) is specifically "tooling an Onboarding Manager uses
-- day to day" -- audit_log/tool_runs/vendor_format/qms_tag/tag_pattern
-- all genuinely belong there by that same definition; these two
-- admin-only integration-credential tables never did.
--
-- ALTER TABLE ... SET SCHEMA preserves RLS policies, triggers,
-- indexes, and FK constraints unchanged -- only the table's own
-- namespace moves. Both tables' RLS policies reference only
-- auth.current_user_has_role(...), independent of either schema, so
-- nothing there needs to change.
CREATE SCHEMA IF NOT EXISTS integrations;

ALTER TABLE client_ops.dropbox_configuration SET SCHEMA integrations;
ALTER TABLE client_ops.process_street_settings SET SCHEMA integrations;
