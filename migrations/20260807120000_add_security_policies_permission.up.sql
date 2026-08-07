-- Security Policies (Administration > Security Policies) is its own
-- admin capability, distinct from users.manage/users.manage_roles/
-- audit_logs.read/roles.manage -- editing org-wide auth policy
-- (auth.auth_configuration) is a different concern from any of those.
-- Purely additive: a new permission row plus a grant to admin, exactly
-- the kind of catalog growth the original seed migration's own comment
-- anticipated ("content is expected to grow as new gated actions get
-- built").
INSERT INTO auth.permissions (key, label, description) VALUES
    ('security_policies.manage', 'Manage security policies', 'View and edit org-wide authentication policy (auth.auth_configuration) -- currently step-up requirements.');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT id, 'security_policies.manage' FROM auth.roles WHERE key = 'admin';

-- Anticipated in Roles & Permissions design discussion comments back when
-- ROLE_CHANGED still existed as a single event ("...and any future
-- role_changed/auth_configuration_updated"); this is that event, now that
-- Security Policies actually has something to audit.
