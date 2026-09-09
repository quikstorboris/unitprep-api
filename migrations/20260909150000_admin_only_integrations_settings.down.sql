DROP TABLE IF EXISTS client_ops.dropbox_configuration;

DROP POLICY IF EXISTS process_street_settings_update_admin_only
    ON client_ops.process_street_settings;

CREATE POLICY process_street_settings_update_client_ops_roles
    ON client_ops.process_street_settings FOR UPDATE
    USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'))
    WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
