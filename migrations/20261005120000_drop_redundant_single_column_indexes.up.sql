-- Efficiency Refactor E3: drop four single-column indexes that a UNIQUE
-- constraint on the same table already covers.
--
-- Each of these indexes is on the leading column of a composite UNIQUE
-- index the table also has (the UNIQUE constraint's own index), and a btree
-- index on (a, b, ...) serves every lookup, join and ordered scan on (a)
-- alone -- so the single-column twin adds nothing to reads while still
-- costing space and a write on every insert/update/delete:
--
--   clients.facility_merchant_account_parties (facility_id)
--       covered by UNIQUE (facility_id, party_role, party_index)
--   clients.policy_coverage_tiers (facility_policies_id)
--       covered by UNIQUE (facility_policies_id, tier_number)
--   clients.policy_delinquency_steps (facility_policies_id)
--       covered by UNIQUE (facility_policies_id, step_order)
--   clients.ps_task_status (facility_id)
--       covered by UNIQUE (facility_id, workflow, ps_task_id)
--
-- Verified against the migrated schema (pg_indexes), not just the migration
-- text. Foreign keys on these columns are unaffected: Postgres does not
-- require an index on a referencing column, and the composite UNIQUE index
-- serves the cascade lookups just as the dropped one did.
DROP INDEX IF EXISTS clients.facility_merchant_account_parties_facility_id_idx;
DROP INDEX IF EXISTS clients.policy_coverage_tiers_facility_policies_id_idx;
DROP INDEX IF EXISTS clients.policy_delinquency_steps_facility_policies_id_idx;
DROP INDEX IF EXISTS clients.ps_task_status_facility_id_idx;
