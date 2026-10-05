-- Efficiency Refactor A3: index the Process Street run-id columns that are
-- looked up with `= ANY($1)`.
--
-- Every client search, import preview and "create from Process Street"
-- check asks "which of these PS run ids are already imported?":
--   * clients_search.rs  -- facilities.ps_intake_run_id and
--                           facility_merchant_accounts.ps_new_merchant_run_id
--   * clients/create.rs  -- facilities.ps_intake_run_id and
--                           companies.ps_intake_run_id
-- None of the three columns had an index, so each of those lookups was a
-- sequential scan. Invisible at today's row counts; it grows linearly
-- with every client onboarded.
--
-- Partial (`WHERE ... IS NOT NULL`) because manually-created rows carry no
-- run id: the index only holds rows that can ever match, and a plain
-- equality / `= ANY` predicate implies NOT NULL, so the planner still
-- uses it. Deliberately NOT unique: nothing in the data model forbids two
-- rows pointing at one run (a sister-facility import can), and a unique
-- index here would turn that into an insert failure.
CREATE INDEX facilities_ps_intake_run_id_idx
    ON clients.facilities (ps_intake_run_id)
    WHERE ps_intake_run_id IS NOT NULL;

CREATE INDEX companies_ps_intake_run_id_idx
    ON clients.companies (ps_intake_run_id)
    WHERE ps_intake_run_id IS NOT NULL;

CREATE INDEX facility_merchant_accounts_ps_new_merchant_run_id_idx
    ON clients.facility_merchant_accounts (ps_new_merchant_run_id)
    WHERE ps_new_merchant_run_id IS NOT NULL;
