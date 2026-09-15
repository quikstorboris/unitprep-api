-- The "avoid hardcoding a person's raw PS text forever" mechanism for
-- Implementation Manager/Sales Rep resolution (see
-- 20260911120000_add_staff_assignments_to_companies and
-- src/clients/staff_resolution.rs). A raw identifier PS gives us (an
-- email for "Who is the conductor", a name for "Fill in Rep Only
-- fields") is checked against this table BEFORE falling back to a
-- direct auth.users match -- the one sanctioned way to redirect a raw
-- identifier that no longer maps correctly (a former employee, a typo'd
-- name PS keeps repeating on every copy-pasted run) without silently
-- mismatching or hardcoding the redirect in Rust. Also becomes the
-- backing store for the deferred self-service "Reassign Implementation
-- Manager" feature (Phase 2): add/replace an alias row plus a bulk
-- update of existing company rows pointing at the old value is the same
-- mechanism, just with a UI on top later.
CREATE TABLE clients.staff_identity_alias (
    id BIGSERIAL PRIMARY KEY,
    raw_identifier TEXT UNIQUE NOT NULL,
    resolved_user_id UUID NOT NULL REFERENCES auth.users(id),
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE clients.staff_identity_alias ENABLE ROW LEVEL SECURITY;

-- Read: any authenticated caller -- this table is consulted by ordinary
-- staff-resolution logic (not just admins), and holds no secret, just a
-- historical text-to-user redirect. Write: admin only for now (mirrors
-- client_ops.dropbox_configuration's admin-only pair) -- there is no
-- self-service UI yet, so the only writer today is this migration's own
-- seed row below.
CREATE POLICY staff_identity_alias_select_authenticated ON clients.staff_identity_alias
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY staff_identity_alias_insert_admin_only ON clients.staff_identity_alias
    FOR INSERT
    WITH CHECK (auth.current_user_has_role('admin'));

CREATE POLICY staff_identity_alias_update_admin_only ON clients.staff_identity_alias
    FOR UPDATE
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));

CREATE POLICY staff_identity_alias_delete_admin_only ON clients.staff_identity_alias
    FOR DELETE
    USING (auth.current_user_has_role('admin'));

-- Ian Masse (a former Quikstor employee) shows up as a historical
-- conductor on old Intake runs; resolves to Boris going forward. Same
-- `WHERE u.email = 'bmaksimov@quikstor.com'` subquery style as
-- migrate_users_role_to_user_roles's own Boris-account grant.
INSERT INTO clients.staff_identity_alias (raw_identifier, resolved_user_id, note)
SELECT 'imasse@quikstor.com', u.id,
       'Former employee (Ian Masse) -- historical Intake conductor, resolves to Boris going forward.'
FROM auth.users u
WHERE u.email = 'bmaksimov@quikstor.com';

-- Narrow, safe cross-RLS-boundary read for staff display/matching.
-- auth.users' own SELECT policy (users_select_own_or_admin) is
-- deliberately restrictive (a caller may only see their own row, or
-- every row if admin) -- correct for that table's general PII (job
-- title, company, status), but it structurally blocks two things this
-- feature needs from an ordinary (non-admin) caller: showing a
-- company's assigned Implementation Manager/Sales Rep's *name* on the
-- clients list (any authenticated caller's own read), and
-- staff_resolution.rs matching a raw PS identifier against *any* user's
-- email/name (this runs during company creation, gated by
-- client_ops.perform, held by onboarding_manager/department_manager --
-- neither of which is admin).
--
-- Rather than widening auth.users' own RLS policy (a much bigger surface
-- than this feature needs), this SECURITY DEFINER function exposes only
-- id/email/first_name/last_name -- enough to display a name and match an
-- identifier, nothing else (no job_title, company, status, role). Same
-- "authenticated, not admin" gate as clients.*'s own *_select_authenticated
-- policies -- a plain GUC-presence check, not an exception: an
-- unauthenticated caller (the GUC unset) just gets zero rows back,
-- mirroring how RLS itself behaves for those policies.
CREATE FUNCTION auth.staff_directory()
RETURNS TABLE (id UUID, email TEXT, first_name TEXT, last_name TEXT)
LANGUAGE sql
SECURITY DEFINER
SET search_path = auth, public
AS $$
    SELECT u.id, u.email::text, u.first_name, u.last_name
      FROM auth.users u
     WHERE u.deleted_at IS NULL
       AND NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL;
$$;

REVOKE EXECUTE ON FUNCTION auth.staff_directory() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.staff_directory() TO app_service;
