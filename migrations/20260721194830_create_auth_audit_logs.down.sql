DROP TRIGGER IF EXISTS auth_audit_logs_no_delete ON auth_audit_logs;
DROP TRIGGER IF EXISTS auth_audit_logs_no_update ON auth_audit_logs;
DROP FUNCTION IF EXISTS prevent_audit_log_mutation();
DROP TABLE IF EXISTS auth_audit_logs;
