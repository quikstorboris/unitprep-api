ALTER TABLE auth_configuration ENABLE ROW LEVEL SECURITY;

CREATE POLICY auth_configuration_admin_only ON auth_configuration
    FOR ALL
    USING (current_setting('app.current_user_role', true) = 'admin')
    WITH CHECK (current_setting('app.current_user_role', true) = 'admin');
