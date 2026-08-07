-- Authorization data model: roles/permissions become real, queryable data
-- (auth.roles, auth.permissions, auth.role_permissions) rather than a
-- hardcoded map in Rust, and a user can hold more than one role at once
-- (auth.user_roles, many-to-many) rather than the single auth.users.role
-- column. Deliberately additive and safe to run on its own: nothing here
-- changes what the currently-running app reads or writes. The actual
-- cutover -- migrating existing role data, rewriting the admin-check
-- functions/policies that still reference the old column and GUC, and
-- dropping auth.users.role -- is the companion migration
-- (migrate_users_role_to_user_roles) that follows immediately after.
--
-- Lives in the auth schema, not public: this is the authorization half of
-- identity, same domain and same request lifecycle as auth.users/
-- auth.sessions, not a new business concern that would warrant its own
-- schema (see the Postgres-schemas-per-domain rule).

CREATE TABLE auth.roles (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    key TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    description TEXT,
    -- Protects the four roles the permission model itself depends on
    -- existing (admin, onboarding_manager, district_manager, sales) from
    -- being deleted or renamed out from under it. Custom roles an admin
    -- creates later are is_system = false and fully manageable -- this is
    -- not a general anti-deletion stance, only a guard on the built-ins.
    is_system BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE auth.permissions (
    -- Natural text key, not a synthetic id: a permission's key is its
    -- stable identity (checked by string in application code, e.g.
    -- has_permission(user, "client_ops.perform")), defined by the
    -- codebase as gated actions are built, never renamed through any
    -- admin UI the way a role's label might be.
    key TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE auth.role_permissions (
    role_id UUID NOT NULL REFERENCES auth.roles(id) ON DELETE CASCADE,
    permission_key TEXT NOT NULL REFERENCES auth.permissions(key) ON DELETE CASCADE,
    PRIMARY KEY (role_id, permission_key)
);

CREATE TABLE auth.user_roles (
    user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    role_id UUID NOT NULL REFERENCES auth.roles(id) ON DELETE CASCADE,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    granted_by UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    PRIMARY KEY (user_id, role_id)
);

CREATE INDEX idx_user_roles_role_id ON auth.user_roles(role_id);

-- Cheap, GUC-only role-membership check for use inside RLS policies and
-- the admin-check functions below -- deliberately NOT a live query
-- against user_roles/roles. This continues the exact reasoning the
-- original single-role design already used ("avoiding a self-referential
-- subquery back into users to check is this caller admin" -- see RLS
-- Implementation's session-variable convention): app.current_user_roles
-- is a comma-joined list of the caller's role keys, set once per request
-- by the same middleware that resolves the session (authenticated_user.rs),
-- from resolve_session's now-plural role_keys output. A role change takes
-- effect on the caller's NEXT request, same as today, since nothing here
-- caches across requests.
CREATE FUNCTION auth.current_user_has_role(p_role_key TEXT)
RETURNS BOOLEAN
LANGUAGE sql
STABLE
AS $$
    SELECT p_role_key = ANY(
        string_to_array(current_setting('app.current_user_roles', true), ',')
    );
$$;

REVOKE EXECUTE ON FUNCTION auth.current_user_has_role(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.current_user_has_role(TEXT) TO app_service;

ALTER TABLE auth.roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE auth.permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE auth.role_permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE auth.user_roles ENABLE ROW LEVEL SECURITY;

-- Role/permission/mapping definitions are catalog data, not per-user
-- secrets -- any authenticated caller (any resolved session) can read
-- them, e.g. to render a role's label or the capability matrix. Only
-- mutation is admin-gated.
CREATE POLICY roles_select_authenticated ON auth.roles
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);
CREATE POLICY roles_insert_admin_only ON auth.roles
    FOR INSERT
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY roles_update_admin_only ON auth.roles
    FOR UPDATE
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));
-- is_system = false in the USING clause is a DB-layer backstop on top of
-- the app-layer rule -- a built-in role cannot be deleted even by a bug in
-- the future custom-role-editor UI.
CREATE POLICY roles_delete_admin_only_non_system ON auth.roles
    FOR DELETE
    USING (auth.current_user_has_role('admin') AND is_system = false);

CREATE POLICY permissions_select_authenticated ON auth.permissions
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);
CREATE POLICY permissions_insert_admin_only ON auth.permissions
    FOR INSERT
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY permissions_update_admin_only ON auth.permissions
    FOR UPDATE
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY permissions_delete_admin_only ON auth.permissions
    FOR DELETE
    USING (auth.current_user_has_role('admin'));

CREATE POLICY role_permissions_select_authenticated ON auth.role_permissions
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);
CREATE POLICY role_permissions_insert_admin_only ON auth.role_permissions
    FOR INSERT
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY role_permissions_delete_admin_only ON auth.role_permissions
    FOR DELETE
    USING (auth.current_user_has_role('admin'));

-- user_roles is the sensitive one: a user's own role assignments are
-- visible to themselves and to admin, and -- the self-role-edit-absolute
-- rule, now enforced at the DB layer as well as the app layer -- nobody
-- may grant or revoke a role on their OWN account through this path,
-- admin included. The one sanctioned way around that is a migration
-- running as the table owner (bypasses RLS entirely), which is exactly
-- how Boris's own two roles get seeded in the companion migration.
CREATE POLICY user_roles_select_own_or_admin ON auth.user_roles
    FOR SELECT
    USING (
        user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR auth.current_user_has_role('admin')
    );
CREATE POLICY user_roles_insert_admin_only ON auth.user_roles
    FOR INSERT
    WITH CHECK (
        auth.current_user_has_role('admin')
        AND user_id <> NULLIF(current_setting('app.current_user_id', true), '')::uuid
    );
CREATE POLICY user_roles_delete_admin_only ON auth.user_roles
    FOR DELETE
    USING (
        auth.current_user_has_role('admin')
        AND user_id <> NULLIF(current_setting('app.current_user_id', true), '')::uuid
    );

-- Seed the four system roles.
INSERT INTO auth.roles (key, label, description, is_system) VALUES
    ('admin', 'Admin', 'System and user administration. Never performs or approves client operations.', true),
    ('onboarding_manager', 'Onboarding Manager', 'Performs client operations (dedup, unit-group, etc.) and manages QMS credentials for clients they work on.', true),
    ('district_manager', 'District Manager', 'Everything Onboarding Manager can do, plus approving pending QMS credential changes.', true),
    ('sales', 'Sales', 'Read-only access to client records. Capabilities still being defined.', true);

-- Seed the permission catalog as enumerated from the 2026-08-06 capability
-- matrix discussion. Content is expected to grow as new gated actions get
-- built -- this list is a starting point, not a final one, and growing it
-- later is a data change, not a migration.
INSERT INTO auth.permissions (key, label, description) VALUES
    ('users.manage', 'Manage users', 'Invite, deactivate, and edit other users'' basic profile fields.'),
    ('users.manage_roles', 'Manage user roles', 'Assign or revoke roles for another user. Never usable on one''s own account.'),
    ('audit_logs.read', 'Read audit logs', 'View the system/security audit log (Administration > Audit Logs).'),
    ('roles.manage', 'Manage roles', 'Create, edit, or delete custom roles and their permissions. Reserved -- no admin UI exists yet.'),
    ('client_ops.perform', 'Perform client operations', 'Run client-facing tools such as dedup and unit-group analysis.'),
    ('client_credentials.add', 'Add client API credentials', 'Add a QMS API credential for a client.'),
    ('client_credentials.revoke', 'Revoke client API credentials', 'Revoke a QMS API credential for a client.'),
    ('client_credentials.approve', 'Approve client API credential changes', 'Approve a pending request to add a client''s QMS API credential.');

-- Role -> permission grants, per the same matrix. Assumptions worth
-- flagging rather than burying: district_manager is given
-- client_credentials.add directly (not just approve), inferred from "OM
-- not allowed: elevated privileges of DM's" implying DM's client-ops
-- capabilities are a superset of OM's -- not something Boris stated
-- explicitly. sales gets nothing yet (capabilities deferred). Both are one
-- UPDATE away from correct if wrong.
INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT r.id, p.key FROM auth.roles r, auth.permissions p
WHERE (r.key = 'admin' AND p.key IN ('users.manage', 'users.manage_roles', 'audit_logs.read', 'roles.manage'))
   OR (r.key = 'onboarding_manager' AND p.key IN ('client_ops.perform', 'client_credentials.add', 'client_credentials.revoke'))
   OR (r.key = 'district_manager' AND p.key IN ('client_ops.perform', 'client_credentials.add', 'client_credentials.revoke', 'client_credentials.approve'));
