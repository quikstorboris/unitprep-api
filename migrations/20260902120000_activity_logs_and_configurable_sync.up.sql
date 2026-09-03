-- Three additive pieces of the same 2026-09-02 conversation, bundled in
-- one migration since they ship together:
--
-- 1. `activity_logs.read` -- a new permission gating the "Activity Logs"
--    admin page (client_ops.audit_log's first-ever UI, alongside the
--    existing "Security Logs" rename of what was "Audit Logs"). Kept
--    distinct from `audit_logs.read` on purpose: that one gates the
--    security audit trail (logins, role changes) and stays admin-only;
--    this one is an operations trail for the people doing the
--    operations, so it's granted to the same three client-ops-adjacent
--    roles that can already SELECT client_ops.audit_log under RLS
--    (see 20260808130000's own comment) -- this permission is the
--    app-layer mirror of that existing RLS grant, not a widening of it.
--
-- 2. `sync_interval_hours` replaces `sync_time` on
--    `client_ops.process_street_settings` -- Boris's call: a single
--    fixed daily clock time undersells what the sync's own delta
--    mechanism (`clients::sync`'s own doc comment) can actually do
--    cheaply. An interval in hours lets the same background job run far
--    more often without a proportional cost increase, since an unchanged
--    run still costs nothing beyond the one shared list call. Default 24
--    preserves today's effective cadence for anyone who hasn't touched
--    the setting yet.
--
-- 3. `manually_edited_fields` on `clients.companies`/`clients.facilities`
--    -- tracks which fields a human has deliberately set to a value that
--    may now differ from a fresh Process Street pull, so a future sync
--    (scheduled or the scoped "Re-sync" button) never silently clobbers
--    a real correction. Populated for the first time at Create (see
--    `clients::create`), extended later once the client detail page
--    supports post-creation editing.
INSERT INTO auth.permissions (key, label, description) VALUES
    ('activity_logs.read', 'View activity logs', 'View the client-ops activity trail (imports, dedup/unit-group runs, syncs) -- distinct from the security audit trail.');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT r.id, 'activity_logs.read' FROM auth.roles r
WHERE r.key IN ('admin', 'onboarding_manager', 'department_manager');

ALTER TABLE client_ops.process_street_settings
    ADD COLUMN sync_interval_hours SMALLINT NOT NULL DEFAULT 24
        CHECK (sync_interval_hours BETWEEN 1 AND 168);

ALTER TABLE client_ops.process_street_settings
    DROP COLUMN sync_time;

ALTER TABLE clients.companies
    ADD COLUMN manually_edited_fields TEXT[] NOT NULL DEFAULT '{}';

ALTER TABLE clients.facilities
    ADD COLUMN manually_edited_fields TEXT[] NOT NULL DEFAULT '{}';
