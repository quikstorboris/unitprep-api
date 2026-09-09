-- Integrations (Process Street, Dropbox) become an admin-only nav
-- section and settings pages, per Boris's explicit call: unlike
-- client_ops.perform (which admin deliberately never holds -- see
-- client_ops.audit_log's own migration comment on that separation of
-- duties), configuring a third-party integration's own
-- credentials/schedule is being treated as system administration, not a
-- client operation.
--
-- New permission rather than reusing client_ops.perform or an existing
-- admin permission: distinct concern from users/roles/audit-logs/
-- security policies, and it now also gates the pre-existing Process
-- Street settings endpoint, which previously required client_ops.perform
-- (see the companion migration moving that gate, and unitprep-ui's
-- LeftNav.tsx/process-street page.tsx).
INSERT INTO auth.permissions (key, label, description) VALUES
    ('integrations.manage', 'Manage integrations', 'View and edit configuration for third-party integrations (Process Street, Dropbox).');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT id, 'integrations.manage' FROM auth.roles WHERE key = 'admin';
