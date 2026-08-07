-- Widens client_ops.qms_tag maintenance from admin-only to all three
-- roles that hold client_ops-adjacent permissions today (admin,
-- onboarding_manager, department_manager) -- Boris's call: this catalog
-- is a stand-in for QMS one day exposing these tags via its own API, and
-- restricting who can keep it current serves no purpose while it's a
-- hand-maintained bridge.
--
-- Also adds client_ops.audit_log: a distinct, non-security operations
-- trail for client-ops mutations (this migration's own qms_tag edits
-- today; client-credential adds/revokes and similar client-ops actions
-- later). Deliberately separate from auth.auth_audit_logs -- same
-- reasoning already settled for keeping Admin's oversight audit and
-- client-ops business data apart: one filtered-by-column table would
-- blur a boundary that is meant to stay structural.

-- A new permission rather than reusing client_ops.perform: admin
-- deliberately does not hold client_ops.perform ("never performs or
-- approves client operations" -- see add_roles_permissions_tables).
-- Maintaining a reference catalog of QMS tag names is closer to system
-- configuration than to performing a client operation, so it gets its
-- own narrow permission all three roles can hold without blurring that
-- boundary.
INSERT INTO auth.permissions (key, label, description) VALUES
    ('client_ops.manage_tags', 'Manage QMS tag catalog', 'Add, edit, or deactivate entries in the hand-maintained QMS template-tag catalog.');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT r.id, 'client_ops.manage_tags' FROM auth.roles r
WHERE r.key IN ('admin', 'onboarding_manager', 'department_manager');

-- Widen the RLS policies themselves to match -- the permission grant
-- above gates the HTTP endpoints; these are the DB-level backstop, and
-- must agree with it or the backstop silently contradicts the app layer.
DROP POLICY qms_tag_insert_admin_only ON client_ops.qms_tag;
DROP POLICY qms_tag_update_admin_only ON client_ops.qms_tag;
DROP POLICY qms_tag_delete_admin_only ON client_ops.qms_tag;

CREATE POLICY qms_tag_insert_client_ops_roles ON client_ops.qms_tag
    FOR INSERT
    WITH CHECK (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY qms_tag_update_client_ops_roles ON client_ops.qms_tag
    FOR UPDATE
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    )
    WITH CHECK (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY qms_tag_delete_client_ops_roles ON client_ops.qms_tag
    FOR DELETE
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );

-- client_ops.audit_log: append-only, same shape/reasoning as
-- auth.auth_audit_logs (see src/auth/audit_log.rs's module doc) --
-- infallible-from-the-caller writes, no RETURNING, before/after JSONB
-- for value transitions. entity_type/entity_id are generic (text, not a
-- foreign key) because this log is meant to outlive qms_tag as the only
-- thing it records -- client credentials and other client-ops mutations
-- land here too, and those entities don't all share one key type.
CREATE TABLE client_ops.audit_log (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    event_type TEXT NOT NULL,
    actor_user_id UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT,
    before_state JSONB,
    after_state JSONB,
    user_agent TEXT,
    ip_address INET,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_client_ops_audit_log_entity ON client_ops.audit_log(entity_type, entity_id);
CREATE INDEX idx_client_ops_audit_log_created_at ON client_ops.audit_log(created_at);

ALTER TABLE client_ops.audit_log ENABLE ROW LEVEL SECURITY;

-- Unconditional insert, same reasoning as auth_audit_logs: a write must
-- never be blocked by the identity context of the moment, or a logging
-- gap turns into an operation-blocking bug. In practice every write here
-- will carry a real actor_user_id, since only an authenticated, permitted
-- caller ever reaches the code path that writes one.
CREATE POLICY client_ops_audit_log_insert_unconditional ON client_ops.audit_log
    FOR INSERT
    WITH CHECK (true);

-- Readable by the same three roles that can write client-ops data --
-- this is an operations trail for the people doing the operations, not
-- an oversight tool restricted to admin the way auth.auth_audit_logs is.
CREATE POLICY client_ops_audit_log_select_client_ops_roles ON client_ops.audit_log
    FOR SELECT
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );

-- No UPDATE/DELETE policy at all -- RLS default-denies both, and the
-- blanket schema-level grant in setup_app_service_role.sql is narrowed
-- right back down for this table specifically, same append-only pattern
-- as auth.auth_audit_logs (see that script's own re-assertion block).
