ALTER TABLE auth.auth_audit_logs
    DROP CONSTRAINT auth_audit_logs_actor_user_id_fkey,
    ADD CONSTRAINT auth_audit_logs_actor_user_id_fkey
        FOREIGN KEY (actor_user_id) REFERENCES auth.users(id) ON DELETE SET NULL;

ALTER TABLE auth.auth_audit_logs
    DROP CONSTRAINT auth_audit_logs_target_user_id_fkey,
    ADD CONSTRAINT auth_audit_logs_target_user_id_fkey
        FOREIGN KEY (target_user_id) REFERENCES auth.users(id) ON DELETE SET NULL;
