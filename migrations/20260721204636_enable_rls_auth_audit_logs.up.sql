ALTER TABLE auth_audit_logs ENABLE ROW LEVEL SECURITY;

CREATE POLICY auth_audit_logs_select_admin_only ON auth_audit_logs
    FOR SELECT
    USING (current_setting('app.current_user_role', true) = 'admin');

CREATE POLICY auth_audit_logs_insert_always ON auth_audit_logs
    FOR INSERT
    WITH CHECK (true);
