DROP TABLE IF EXISTS clients.company_clickup_parent_history;

ALTER TABLE clients.companies
    DROP COLUMN IF EXISTS clickup_waived_by,
    DROP COLUMN IF EXISTS clickup_waived_at,
    DROP COLUMN IF EXISTS clickup_parent_facility_id;
