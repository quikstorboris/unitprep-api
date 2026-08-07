DROP TABLE IF EXISTS client_ops.audit_log;

DROP POLICY IF EXISTS qms_tag_delete_client_ops_roles ON client_ops.qms_tag;
DROP POLICY IF EXISTS qms_tag_update_client_ops_roles ON client_ops.qms_tag;
DROP POLICY IF EXISTS qms_tag_insert_client_ops_roles ON client_ops.qms_tag;

CREATE POLICY qms_tag_insert_admin_only ON client_ops.qms_tag
    FOR INSERT
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY qms_tag_update_admin_only ON client_ops.qms_tag
    FOR UPDATE
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY qms_tag_delete_admin_only ON client_ops.qms_tag
    FOR DELETE
    USING (auth.current_user_has_role('admin'));

DELETE FROM auth.role_permissions WHERE permission_key = 'client_ops.manage_tags';
DELETE FROM auth.permissions WHERE key = 'client_ops.manage_tags';
