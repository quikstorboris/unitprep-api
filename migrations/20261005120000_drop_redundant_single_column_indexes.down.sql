CREATE INDEX facility_merchant_account_parties_facility_id_idx
    ON clients.facility_merchant_account_parties (facility_id);
CREATE INDEX policy_coverage_tiers_facility_policies_id_idx
    ON clients.policy_coverage_tiers (facility_policies_id);
CREATE INDEX policy_delinquency_steps_facility_policies_id_idx
    ON clients.policy_delinquency_steps (facility_policies_id);
CREATE INDEX ps_task_status_facility_id_idx
    ON clients.ps_task_status (facility_id);
