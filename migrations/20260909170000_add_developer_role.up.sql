-- New system role: developer. Boris's spec, verbatim: "like admin, this
-- role will have full access to both integration configs, and logs. No
-- access to security configs/logs unless admin role is assigned along
-- side developer. They will also have full access to everything that
-- onboarding manager has."
--
-- Permission-catalog grants are the easy half (below). The hard half is
-- that this codebase's RLS layer does NOT check permissions -- every
-- `clients.*`/`client_ops.*` table's own INSERT/UPDATE/DELETE policies
-- hardcode `auth.current_user_has_role('onboarding_manager') OR
-- auth.current_user_has_role('department_manager')` directly (69 policies
-- across 23 tables, confirmed by querying `pg_policies` directly rather
-- than trying to reconstruct "current state" from migration history by
-- hand). `require_permission("client_ops.perform", ...)` in the Rust
-- handlers is the UX layer; these policies are the real enforcement --
-- see this same reasoning already stated in
-- `20260831160000_create_process_street_settings`'s own comment. Simply
-- granting `developer` the `client_ops.perform`/`client_ops.manage_tags`
-- permissions in the catalog below would pass every app-layer check and
-- then silently fail at the database, exactly the bug
-- `20260909150000_admin_only_integrations_settings` fixed for Process
-- Street's own settings table.
--
-- Rather than hand-editing 69 near-identical OR-chains (and repeating
-- that exercise for whichever role needs this same access next), this
-- introduces one shared function, `auth.current_user_is_client_ops_role()`,
-- and repoints every one of those 69 policies at it. Adding a role to
-- this club going forward is a one-line change to the function, not
-- another 69-policy sweep.

INSERT INTO auth.roles (key, label, description, is_system) VALUES
    ('developer', 'Developer', 'Full access to integration configs and Activity Logs, plus everything Onboarding Manager can do. No Security Logs/Policies or user administration unless the admin role is also assigned.', true);

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT r.id, p.key
FROM auth.roles r, auth.permissions p
WHERE r.key = 'developer'
  AND p.key IN (
    'integrations.manage',
    'activity_logs.read',
    'client_ops.perform',
    'client_ops.manage_tags',
    'client_credentials.add',
    'client_credentials.revoke'
  );

-- The RLS-layer equivalent of "everything onboarding_manager/
-- department_manager can do" -- see this migration's own top comment.
CREATE FUNCTION auth.current_user_is_client_ops_role()
RETURNS BOOLEAN
LANGUAGE sql
STABLE
AS $$
    SELECT auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
        OR auth.current_user_has_role('developer');
$$;

REVOKE EXECUTE ON FUNCTION auth.current_user_is_client_ops_role() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.current_user_is_client_ops_role() TO app_service;

ALTER POLICY client_ops_audit_log_select_client_ops_roles ON client_ops.audit_log USING (auth.current_user_is_client_ops_role());
ALTER POLICY qms_tag_delete_client_ops_roles ON client_ops.qms_tag USING (auth.current_user_is_client_ops_role());
ALTER POLICY qms_tag_insert_client_ops_roles ON client_ops.qms_tag WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY qms_tag_update_client_ops_roles ON client_ops.qms_tag USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY tag_pattern_delete_client_ops_roles ON client_ops.tag_pattern USING (auth.current_user_is_client_ops_role());
ALTER POLICY tag_pattern_insert_client_ops_roles ON client_ops.tag_pattern WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY tag_pattern_update_client_ops_roles ON client_ops.tag_pattern USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY vendor_format_delete_client_ops_roles ON client_ops.vendor_format USING (auth.current_user_is_client_ops_role());
ALTER POLICY vendor_format_insert_client_ops_roles ON client_ops.vendor_format WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY vendor_format_update_client_ops_roles ON client_ops.vendor_format USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY companies_delete_client_ops_roles ON clients.companies USING (auth.current_user_is_client_ops_role());
ALTER POLICY companies_insert_client_ops_roles ON clients.companies WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY companies_update_client_ops_roles ON clients.companies USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facilities_delete_client_ops_roles ON clients.facilities USING (auth.current_user_is_client_ops_role());
ALTER POLICY facilities_insert_client_ops_roles ON clients.facilities WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facilities_update_client_ops_roles ON clients.facilities USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_contract_orders_delete_client_ops_roles ON clients.facility_contract_orders USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_contract_orders_insert_client_ops_roles ON clients.facility_contract_orders WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_contract_orders_update_client_ops_roles ON clients.facility_contract_orders USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_account_parties_delete_client_ops_roles ON clients.facility_merchant_account_parties USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_account_parties_insert_client_ops_roles ON clients.facility_merchant_account_parties WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_account_parties_select_client_ops_roles ON clients.facility_merchant_account_parties USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_account_parties_update_client_ops_roles ON clients.facility_merchant_account_parties USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_accounts_delete_client_ops_roles ON clients.facility_merchant_accounts USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_accounts_insert_client_ops_roles ON clients.facility_merchant_accounts WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_accounts_select_client_ops_roles ON clients.facility_merchant_accounts USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_merchant_accounts_update_client_ops_roles ON clients.facility_merchant_accounts USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_people_delete_client_ops_roles ON clients.facility_people USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_people_insert_client_ops_roles ON clients.facility_people WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_people_update_client_ops_roles ON clients.facility_people USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_policies_delete_client_ops_roles ON clients.facility_policies USING (auth.current_user_is_client_ops_role());
ALTER POLICY facility_policies_insert_client_ops_roles ON clients.facility_policies WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY facility_policies_update_client_ops_roles ON clients.facility_policies USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY people_delete_client_ops_roles ON clients.people USING (auth.current_user_is_client_ops_role());
ALTER POLICY people_insert_client_ops_roles ON clients.people WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY people_update_client_ops_roles ON clients.people USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_commission_delete_client_ops_roles ON clients.policy_commission USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_commission_insert_client_ops_roles ON clients.policy_commission WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_commission_update_client_ops_roles ON clients.policy_commission USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_coverage_tiers_delete_client_ops_roles ON clients.policy_coverage_tiers USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_coverage_tiers_insert_client_ops_roles ON clients.policy_coverage_tiers WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_coverage_tiers_update_client_ops_roles ON clients.policy_coverage_tiers USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_delinquency_entries_delete_client_ops_roles ON clients.policy_delinquency_entries USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_delinquency_entries_insert_client_ops_roles ON clients.policy_delinquency_entries WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_delinquency_entries_update_client_ops_roles ON clients.policy_delinquency_entries USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_delinquency_steps_delete_client_ops_roles ON clients.policy_delinquency_steps USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_delinquency_steps_insert_client_ops_roles ON clients.policy_delinquency_steps WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_delinquency_steps_update_client_ops_roles ON clients.policy_delinquency_steps USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_fees_delete_client_ops_roles ON clients.policy_fees USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_fees_insert_client_ops_roles ON clients.policy_fees WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_fees_update_client_ops_roles ON clients.policy_fees USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_specials_delete_client_ops_roles ON clients.policy_specials USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_specials_insert_client_ops_roles ON clients.policy_specials WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_specials_update_client_ops_roles ON clients.policy_specials USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_tax_entries_delete_client_ops_roles ON clients.policy_tax_entries USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_tax_entries_insert_client_ops_roles ON clients.policy_tax_entries WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_tax_entries_update_client_ops_roles ON clients.policy_tax_entries USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_taxes_delete_client_ops_roles ON clients.policy_taxes USING (auth.current_user_is_client_ops_role());
ALTER POLICY policy_taxes_insert_client_ops_roles ON clients.policy_taxes WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY policy_taxes_update_client_ops_roles ON clients.policy_taxes USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY ps_person_index_delete_client_ops_roles ON clients.ps_person_index USING (auth.current_user_is_client_ops_role());
ALTER POLICY ps_person_index_insert_client_ops_roles ON clients.ps_person_index WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY ps_person_index_update_client_ops_roles ON clients.ps_person_index USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY ps_sync_state_delete_client_ops_roles ON clients.ps_sync_state USING (auth.current_user_is_client_ops_role());
ALTER POLICY ps_sync_state_insert_client_ops_roles ON clients.ps_sync_state WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY ps_sync_state_update_client_ops_roles ON clients.ps_sync_state USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY ps_task_status_delete_client_ops_roles ON clients.ps_task_status USING (auth.current_user_is_client_ops_role());
ALTER POLICY ps_task_status_insert_client_ops_roles ON clients.ps_task_status WITH CHECK (auth.current_user_is_client_ops_role());
ALTER POLICY ps_task_status_update_client_ops_roles ON clients.ps_task_status USING (auth.current_user_is_client_ops_role()) WITH CHECK (auth.current_user_is_client_ops_role());

-- The three integrations-secrets policies (`20260909150000_admin_only_
-- integrations_settings`) were written admin-only before `developer`
-- existed. "Full access to integration configs" means developer must
-- actually be able to read/save these, not just pass the app-layer
-- `integrations.manage` permission check and then fail at the database
-- the same way this migration's own top comment describes for
-- client_ops.perform. Inlined rather than a shared function -- only
-- three policies, not sixty-nine.
ALTER POLICY dropbox_configuration_admin_only ON client_ops.dropbox_configuration
    USING (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'));
ALTER POLICY dropbox_configuration_update_admin_only ON client_ops.dropbox_configuration
    USING (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'))
    WITH CHECK (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'));
ALTER POLICY process_street_settings_update_admin_only ON client_ops.process_street_settings
    USING (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'))
    WITH CHECK (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'));

-- Boris's own account gets the new role directly, same sanctioned
-- bootstrap route `20260806130000_migrate_users_role_to_user_roles` used
-- for his original admin+onboarding_manager dual-hat: RLS structurally
-- refuses anyone -- including admin -- from granting or revoking their
-- own roles through the normal app path, so a migration (running as the
-- table owner, bypassing RLS) is the only way.
INSERT INTO auth.user_roles (user_id, role_id)
SELECT u.id, r.id
FROM auth.users u, auth.roles r
WHERE u.email = 'bmaksimov@quikstor.com'
  AND r.key = 'developer'
ON CONFLICT DO NOTHING;
