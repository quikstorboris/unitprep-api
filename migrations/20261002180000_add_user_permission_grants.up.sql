-- Per-user permission grants, on top of role-derived ones.
--
-- Until now a permission could only reach a user through a role
-- (auth.user_roles -> auth.role_permissions). That is the right shape for
-- "what kind of employee is this", and the wrong one for personal
-- integrations such as ClickUp: whether someone should have a ClickUp
-- connection is a per-person decision, and minting a role per integration
-- per team would explode the role list. So a user can now also hold
-- permissions directly.
--
-- Two guards keep this from becoming a privilege-escalation path:
--   1. Only permissions flagged `directly_grantable` may be granted this
--      way (enforced by a trigger, so it holds no matter which code path
--      inserts). users.manage_roles, integrations.manage and friends stay
--      role-only.
--   2. RLS: only admin or department_manager may write grants, and never
--      on their own account.
--
-- `category` exists purely so the Add-permissions dialog can group what it
-- shows; it is NULL for permissions that are not directly grantable.

ALTER TABLE auth.permissions
    ADD COLUMN category TEXT,
    ADD COLUMN directly_grantable BOOLEAN NOT NULL DEFAULT false;

INSERT INTO auth.permissions (key, label, description, category, directly_grantable) VALUES
    ('integrations.clickup', 'ClickUp', 'Connect a personal ClickUp API token and update onboarding tasks from Orchestrator as that user.', 'Integrations', true),
    ('user_permissions.manage', 'Manage user permissions', 'Grant or revoke individually-grantable permissions (such as personal integrations) on another user. Never usable on one''s own account.', NULL, false);

-- Held by admin only for now. Extending this to department_manager is a
-- data change (one role_permissions row) once the Users page itself is
-- opened to that role -- see the ClickUp Integration design log in the
-- vault.
INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT id, 'user_permissions.manage' FROM auth.roles WHERE key = 'admin';

CREATE TABLE auth.user_permissions (
    user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    permission_key TEXT NOT NULL REFERENCES auth.permissions(key) ON DELETE CASCADE,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    granted_by UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    PRIMARY KEY (user_id, permission_key)
);

CREATE INDEX idx_user_permissions_permission_key ON auth.user_permissions (permission_key);

CREATE FUNCTION auth.enforce_directly_grantable() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO 'auth', 'public'
    AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM auth.permissions
         WHERE key = NEW.permission_key AND directly_grantable
    ) THEN
        RAISE EXCEPTION 'permission % cannot be granted directly to a user', NEW.permission_key;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER user_permissions_enforce_directly_grantable
    BEFORE INSERT OR UPDATE ON auth.user_permissions
    FOR EACH ROW
    EXECUTE FUNCTION auth.enforce_directly_grantable();

ALTER TABLE auth.user_permissions ENABLE ROW LEVEL SECURITY;

CREATE POLICY user_permissions_select_own_or_granter ON auth.user_permissions FOR SELECT
    USING (
        user_id = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid
        OR auth.current_user_has_role('admin')
        OR auth.current_user_has_role('department_manager')
    );

CREATE POLICY user_permissions_insert_granter ON auth.user_permissions FOR INSERT
    WITH CHECK (
        (auth.current_user_has_role('admin') OR auth.current_user_has_role('department_manager'))
        AND user_id <> (NULLIF(current_setting('app.current_user_id', true), ''))::uuid
    );

CREATE POLICY user_permissions_delete_granter ON auth.user_permissions FOR DELETE
    USING (
        (auth.current_user_has_role('admin') OR auth.current_user_has_role('department_manager'))
        AND user_id <> (NULLIF(current_setting('app.current_user_id', true), ''))::uuid
    );

-- Session resolution now unions direct grants into the permission set, so
-- every existing `require_permission`/`hasPermission` call site -- and the
-- frontend, which only ever sees the resolved list -- works unchanged.
-- Signature and return shape are identical to the previous definition
-- (20260813150000); only the permission_keys expression changes.
CREATE OR REPLACE FUNCTION auth.resolve_session(p_token_hash bytea, p_idle_minutes integer)
RETURNS TABLE(user_id uuid, role_keys text[], permission_keys text[], elevated_until timestamp with time zone, requires_step_up boolean, passkey_reverified_until timestamp with time zone)
    LANGUAGE sql SECURITY DEFINER
    SET search_path TO 'auth', 'public'
    AS $$
    UPDATE auth.sessions s
    SET last_seen_at = now()
    FROM auth.users u
    WHERE s.token_hash = p_token_hash
      AND s.revoked_at IS NULL
      AND s.expires_at > now()
      AND s.last_seen_at > now() - make_interval(mins => p_idle_minutes)
      AND u.id = s.user_id
      AND u.deleted_at IS NULL
      AND u.status = 'active'
    RETURNING
        u.id,
        (SELECT array_agg(DISTINCT r.key ORDER BY r.key)
           FROM auth.user_roles ur
           JOIN auth.roles r ON r.id = ur.role_id
          WHERE ur.user_id = u.id),
        (SELECT array_agg(DISTINCT k ORDER BY k) FROM (
             SELECT rp.permission_key AS k
               FROM auth.user_roles ur
               JOIN auth.role_permissions rp ON rp.role_id = ur.role_id
              WHERE ur.user_id = u.id
             UNION
             SELECT up.permission_key
               FROM auth.user_permissions up
              WHERE up.user_id = u.id
         ) granted),
        s.elevated_until,
        s.requires_step_up,
        s.passkey_reverified_until;
$$;
