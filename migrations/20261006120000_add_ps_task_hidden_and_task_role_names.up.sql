-- Two related fixes for how the Elavon / Onboarding Summary views read
-- Process Street's merchant-account checklist (2026-10-06, found on the
-- A1 U-Stor-It run):
--
-- 1. Process Street's conditional logic can hide a task from a run
--    (`hidden: true`, `stopped: true` in the tasks API). A 2026-10 template
--    change renamed the credentials step to "Document Credentials" and
--    left the old "Add Credentials to QMS" task in the template but
--    hidden on new runs, so the user never sees it -- yet the sync stored
--    it as an ordinary NotCompleted step, and the Onboarding Summary kept
--    treating it as the live step to wait on. `ps_task_status.hidden`
--    records PS's own flag so readers can ignore tasks a coordinator
--    can't see. Existing rows default to visible until their next resync.
--
-- 2. Which PS task names mean "credentials were added to QMS" was a
--    hardcoded string in five places. Per this repo's data-over-
--    hardcoding principle that is now one admin-editable table of
--    (role, task name) pairs; the code asks for a role, never a name.
--    A role can have several names so older runs (old template) and new
--    runs (renamed task) both resolve, and a future rename is a data
--    edit, not a deploy.
ALTER TABLE clients.ps_task_status
    ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT false;

-- UUID key, not BIGSERIAL: app_service has no sequence grants in the
-- integrations schema (its other tables are singletons, see
-- scripts/setup_app_service_role.sql), and a UUID default needs none.
-- created_at uses clock_timestamp() so rows inserted in one transaction
-- still keep their insertion order.
CREATE TABLE integrations.ps_task_role_name (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role TEXT NOT NULL,
    task_name TEXT NOT NULL CHECK (btrim(task_name) <> ''),
    created_by UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- Names match case-insensitively and ignoring surrounding whitespace,
-- same as the lookups that read this table.
CREATE UNIQUE INDEX ps_task_role_name_role_name_idx
    ON integrations.ps_task_role_name (role, lower(btrim(task_name)));

ALTER TABLE integrations.ps_task_role_name ENABLE ROW LEVEL SECURITY;

-- Readable by any authenticated caller: the Onboarding Summary and the
-- Elavon tab (client_ops roles, sales) resolve names through this table.
-- Mutable by admin only, matching the rest of the integrations schema.
CREATE POLICY ps_task_role_name_select_authenticated ON integrations.ps_task_role_name
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);
CREATE POLICY ps_task_role_name_insert_admin_only ON integrations.ps_task_role_name
    FOR INSERT
    WITH CHECK (auth.current_user_has_role('admin'));
CREATE POLICY ps_task_role_name_delete_admin_only ON integrations.ps_task_role_name
    FOR DELETE
    USING (auth.current_user_has_role('admin'));

INSERT INTO integrations.ps_task_role_name (role, task_name) VALUES
    ('qms_credentials', 'Document Credentials'),
    ('qms_credentials', 'Add Credentials to QMS');
