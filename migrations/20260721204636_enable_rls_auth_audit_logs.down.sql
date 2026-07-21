DROP POLICY IF EXISTS auth_audit_logs_insert_always ON auth_audit_logs;
DROP POLICY IF EXISTS auth_audit_logs_select_admin_only ON auth_audit_logs;
ALTER TABLE auth_audit_logs DISABLE ROW LEVEL SECURITY;
