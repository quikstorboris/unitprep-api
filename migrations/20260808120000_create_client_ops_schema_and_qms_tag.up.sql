-- First schema outside `auth` -- client-ops tooling (dedup, unit-group,
-- the QMS template tagger, eventually client QMS credentials) is a
-- distinct business domain from identity/authorization, the same bar the
-- auth schema's own creation used to justify staying put rather than
-- splitting further (see 20260806120000_add_roles_permissions_tables).
--
-- qms_tag is the first table in it: a small, deliberately incomplete
-- reference catalog of QMS's document-template merge tags, seeded with
-- only the handful of fields common enough to be useful before the full
-- tag list -- and its context-scoping -- is confirmed. Context/category
-- tables and the tag<->context join are intentionally not built yet;
-- adding them later is a normal follow-up migration, not a rework.
--
-- Schema-level USAGE for app_service is granted in
-- scripts/setup_app_service_role.sql, not here -- same split the auth
-- schema uses, and for the same reason: that script must also work when
-- run BEFORE this migration on a fresh branch, at which point this
-- schema doesn't exist yet to grant USAGE on.

CREATE SCHEMA client_ops;

CREATE TABLE client_ops.qms_tag (
    -- Natural key, not a synthetic id -- a tag's key is its stable
    -- identity, matched against literal `{{tag_key}}` text found in a
    -- document, never renamed. Same reasoning as auth.permissions.key.
    tag_key TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    category TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE client_ops.qms_tag ENABLE ROW LEVEL SECURITY;

-- Reference/catalog data, not a per-user secret -- any authenticated
-- caller can read it, same posture as auth.roles/auth.permissions.
CREATE POLICY qms_tag_select_authenticated ON client_ops.qms_tag
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

-- Mutation is admin-only for now, matching the original design's own
-- framing ("admin-only add/edit UI"). Worth revisiting once real usage
-- shows whether onboarding_manager/department_manager -- the roles that
-- actually touch these tags day to day -- should maintain this catalog
-- instead of, or alongside, admin. Not decided; defaulted to the
-- narrower grant since it's the easier one to widen later.
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

-- Seed: the tags QMS's own Default Lease document calls out as its
-- "popular variables" -- deliberately not the full ~300+ tag catalog.
-- See the vault's "QMS Template Tags" note set for provenance and the
-- still-open context-scoping questions.
INSERT INTO client_ops.qms_tag (tag_key, label, category) VALUES
    ('e.fname', 'First Name', 'Tenant'),
    ('e.lname', 'Last Name', 'Tenant'),
    ('e.address', 'Address', 'Tenant'),
    ('e.city', 'City', 'Tenant'),
    ('e.state', 'State', 'Tenant'),
    ('e.post', 'Postal Code', 'Tenant'),
    ('e.phone', 'Phone Number', 'Tenant'),
    ('e.email', 'Email', 'Tenant'),
    ('e.dlnum', 'Driver License Number', 'Tenant'),
    ('e.dlstate', 'Driver License State', 'Tenant'),
    ('u.num', 'Unit Number', 'Unit'),
    ('l.ptd', 'Paid Through Date', 'Lease'),
    ('m.effrate', 'Monthly Rent (In Place Rate)', 'Move-In');
