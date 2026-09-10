ALTER POLICY dropbox_configuration_admin_only ON client_ops.dropbox_configuration
    USING (auth.current_user_has_role('admin'));
ALTER POLICY dropbox_configuration_update_admin_only ON client_ops.dropbox_configuration
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));
ALTER POLICY process_street_settings_update_admin_only ON client_ops.process_street_settings
    USING (auth.current_user_has_role('admin'))
    WITH CHECK (auth.current_user_has_role('admin'));

ALTER POLICY client_ops_audit_log_select_client_ops_roles ON client_ops.audit_log USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY qms_tag_delete_client_ops_roles ON client_ops.qms_tag USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY qms_tag_insert_client_ops_roles ON client_ops.qms_tag WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY qms_tag_update_client_ops_roles ON client_ops.qms_tag USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY tag_pattern_delete_client_ops_roles ON client_ops.tag_pattern USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY tag_pattern_insert_client_ops_roles ON client_ops.tag_pattern WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY tag_pattern_update_client_ops_roles ON client_ops.tag_pattern USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY vendor_format_delete_client_ops_roles ON client_ops.vendor_format USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY vendor_format_insert_client_ops_roles ON client_ops.vendor_format WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY vendor_format_update_client_ops_roles ON client_ops.vendor_format USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY companies_delete_client_ops_roles ON clients.companies USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY companies_insert_client_ops_roles ON clients.companies WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY companies_update_client_ops_roles ON clients.companies USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facilities_delete_client_ops_roles ON clients.facilities USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facilities_insert_client_ops_roles ON clients.facilities WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facilities_update_client_ops_roles ON clients.facilities USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_contract_orders_delete_client_ops_roles ON clients.facility_contract_orders USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_contract_orders_insert_client_ops_roles ON clients.facility_contract_orders WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_contract_orders_update_client_ops_roles ON clients.facility_contract_orders USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_account_parties_delete_client_ops_roles ON clients.facility_merchant_account_parties USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_account_parties_insert_client_ops_roles ON clients.facility_merchant_account_parties WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_account_parties_select_client_ops_roles ON clients.facility_merchant_account_parties USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_account_parties_update_client_ops_roles ON clients.facility_merchant_account_parties USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_accounts_delete_client_ops_roles ON clients.facility_merchant_accounts USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_accounts_insert_client_ops_roles ON clients.facility_merchant_accounts WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_accounts_select_client_ops_roles ON clients.facility_merchant_accounts USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_merchant_accounts_update_client_ops_roles ON clients.facility_merchant_accounts USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_people_delete_client_ops_roles ON clients.facility_people USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_people_insert_client_ops_roles ON clients.facility_people WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_people_update_client_ops_roles ON clients.facility_people USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_policies_delete_client_ops_roles ON clients.facility_policies USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_policies_insert_client_ops_roles ON clients.facility_policies WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY facility_policies_update_client_ops_roles ON clients.facility_policies USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY people_delete_client_ops_roles ON clients.people USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY people_insert_client_ops_roles ON clients.people WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY people_update_client_ops_roles ON clients.people USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_commission_delete_client_ops_roles ON clients.policy_commission USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_commission_insert_client_ops_roles ON clients.policy_commission WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_commission_update_client_ops_roles ON clients.policy_commission USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_coverage_tiers_delete_client_ops_roles ON clients.policy_coverage_tiers USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_coverage_tiers_insert_client_ops_roles ON clients.policy_coverage_tiers WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_coverage_tiers_update_client_ops_roles ON clients.policy_coverage_tiers USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_delinquency_entries_delete_client_ops_roles ON clients.policy_delinquency_entries USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_delinquency_entries_insert_client_ops_roles ON clients.policy_delinquency_entries WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_delinquency_entries_update_client_ops_roles ON clients.policy_delinquency_entries USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_delinquency_steps_delete_client_ops_roles ON clients.policy_delinquency_steps USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_delinquency_steps_insert_client_ops_roles ON clients.policy_delinquency_steps WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_delinquency_steps_update_client_ops_roles ON clients.policy_delinquency_steps USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_fees_delete_client_ops_roles ON clients.policy_fees USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_fees_insert_client_ops_roles ON clients.policy_fees WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_fees_update_client_ops_roles ON clients.policy_fees USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_specials_delete_client_ops_roles ON clients.policy_specials USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_specials_insert_client_ops_roles ON clients.policy_specials WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_specials_update_client_ops_roles ON clients.policy_specials USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_tax_entries_delete_client_ops_roles ON clients.policy_tax_entries USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_tax_entries_insert_client_ops_roles ON clients.policy_tax_entries WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_tax_entries_update_client_ops_roles ON clients.policy_tax_entries USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_taxes_delete_client_ops_roles ON clients.policy_taxes USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_taxes_insert_client_ops_roles ON clients.policy_taxes WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY policy_taxes_update_client_ops_roles ON clients.policy_taxes USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_person_index_delete_client_ops_roles ON clients.ps_person_index USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_person_index_insert_client_ops_roles ON clients.ps_person_index WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_person_index_update_client_ops_roles ON clients.ps_person_index USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_sync_state_delete_client_ops_roles ON clients.ps_sync_state USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_sync_state_insert_client_ops_roles ON clients.ps_sync_state WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_sync_state_update_client_ops_roles ON clients.ps_sync_state USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_task_status_delete_client_ops_roles ON clients.ps_task_status USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_task_status_insert_client_ops_roles ON clients.ps_task_status WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));
ALTER POLICY ps_task_status_update_client_ops_roles ON clients.ps_task_status USING (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager')) WITH CHECK (auth.current_user_has_role('onboarding_manager') OR auth.current_user_has_role('department_manager'));

DROP FUNCTION auth.current_user_is_client_ops_role();

DELETE FROM auth.user_roles
WHERE role_id = (SELECT id FROM auth.roles WHERE key = 'developer');

DELETE FROM auth.role_permissions
WHERE role_id = (SELECT id FROM auth.roles WHERE key = 'developer');

DELETE FROM auth.roles WHERE key = 'developer';
