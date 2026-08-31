-- First setting under the future "Integrations" nav family (Process
-- Street today; Dropbox/ClickUp/Claude etc. are follow-ups per the
-- vault's own design note) -- just the sync schedule for now, per
-- Boris's explicit scope: "only add the Sync Time setting there."
--
-- Singleton row, same shape as auth.auth_configuration: a SMALLINT
-- primary key CHECKed to 1, seeded in this same migration so the app
-- never has to handle a missing row.
--
-- sync_time is a plain TIME, interpreted as UTC by
-- clients::sync::start_background_sync_task -- deliberately not
-- attempting a per-user/per-browser timezone conversion for this first
-- cut; the settings page labels the field "(UTC)" so there's no
-- ambiguity about what it means.
CREATE TABLE client_ops.process_street_settings (
    id SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    sync_time TIME NOT NULL DEFAULT '00:00',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES auth.users(id) ON DELETE SET NULL
);

CREATE TRIGGER process_street_settings_set_updated_at
    BEFORE UPDATE ON client_ops.process_street_settings
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

INSERT INTO client_ops.process_street_settings (id) VALUES (1);

ALTER TABLE client_ops.process_street_settings ENABLE ROW LEVEL SECURITY;

-- Read: any authenticated caller (the settings page needs to show the
-- current value, and the background sync task itself reads this on a
-- system role -- see clients::sync). Write: client_ops.perform, same
-- permission the manual "Sync Now" trigger requires -- this is
-- operational client-ops configuration, not a security policy, so it
-- follows that gate rather than auth.auth_configuration's admin-only
-- one. Admin deliberately does not hold client_ops.perform (see
-- client_ops.audit_log's own migration comment), consistent with every
-- other client-ops write in this app.
CREATE POLICY process_street_settings_select_authenticated
    ON client_ops.process_street_settings FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY process_street_settings_update_client_ops_roles
    ON client_ops.process_street_settings FOR UPDATE
    USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'))
    WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
