-- Follow-up to 20260909170000_add_developer_role: that migration's
-- permission grant was built from `test_support.rs`'s
-- `onboarding_manager_user()` fixture, which turned out to be stale --
-- the real `onboarding_manager` role (seeded 2026-08-18 by
-- `create_vendor_format_registry`) also holds
-- `client_ops.manage_vendor_formats`, confirmed live on `/admin/roles`
-- immediately after applying that migration. "Full access to everything
-- onboarding manager has" means the real role, not the fixture --
-- closing the gap here.
--
-- No RLS work needed alongside this one: `client_ops.vendor_format`'s
-- own INSERT/UPDATE/DELETE policies were already part of the 69-policy
-- sweep in 20260909170000 (they used the same
-- `onboarding_manager OR department_manager` shape as every other
-- client-ops table), so `auth.current_user_is_client_ops_role()`
-- already covers `developer` there.
INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT r.id, 'client_ops.manage_vendor_formats'
FROM auth.roles r
WHERE r.key = 'developer';
