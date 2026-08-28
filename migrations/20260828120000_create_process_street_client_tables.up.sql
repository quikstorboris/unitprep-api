-- OO x Process Street integration, Phase 0 (schema foundations only --
-- no ingestion code, no UI, nothing wired up yet).
--
-- New `clients` schema, not folded into the existing `client_ops` --
-- reconsidered deliberately (Boris's call, 2026-08-28) after an earlier
-- version of this migration briefly lived there. `client_ops` (see
-- 20260808120000_create_client_ops_schema_and_qms_tag) holds
-- tool-support/reference data -- qms_tag, vendor_format, tag_pattern,
-- its own audit_log -- config that makes Group Prep/dedup/the template
-- tagger work. It is small, global (every onboarding manager should see
-- the vendor-format registry regardless of which clients they're
-- assigned to), and slow-growing. What lives below is different in
-- kind: the clients themselves -- companies, facilities, people, their
-- financial/policy configuration -- a much larger, faster-growing
-- domain (14 tables already), and exactly the shape of data a future
-- "Groups" feature (client-scoped visibility -- which OM/DM sees which
-- client, already flagged in the vault as a re-evaluation trigger once
-- real client data exists) would need to scope down to on its own,
-- without also having to carve client-ops's genuinely-global tooling
-- tables out of the same schema. By the same "distinct business domain"
-- bar that justified splitting client_ops out of auth in the first
-- place, this is a second, equally justified split.
--
-- Schema-level USAGE for app_service is granted in
-- scripts/setup_app_service_role.sql, not here -- same split client_ops
-- and auth both use, and for the same reason: that script must also
-- work when run BEFORE this migration on a fresh branch.
--
-- Full design context, decisions, and the real Process Street data this
-- was validated against (Prairie Enterprises / Highway 20 as the golden
-- fixture) live in the vault: work/active/UnitPrep/Process Street
-- Integration/Client & Facility Schema (Process Street-Sourced).md.
--
-- Company/Facility are real business entities (UUID pk, `uuidv7()`
-- default, same convention as auth.users). The five Facility Policies
-- category tables (fees/taxes/delinquency/coverage/commission/specials)
-- are each owned 1:1 by exactly one facility -- there is deliberately no
-- shared/pointed-to row: real Prairie Enterprises data showed Fees/
-- Delinquency/Coverage/Taxes can all vary independently across sister
-- facilities under one company, so nothing here assumes uniformity.
-- "Same for each facility" (copying one facility's category data onto
-- its siblings) is an explicit, application-level, per-category action
-- a human triggers deliberately -- it reuses client_ops.audit_log for
-- its trail (a new event type, ps_facility_policy_copied, added when
-- that UI action is actually built) rather than a new table here, since
-- that append-only trail already exists for exactly this purpose. It
-- staying in client_ops rather than moving to this new schema is
-- deliberate too: audit_log's job is auditing client-ops actions taken
-- against these records, not being one of the records itself.
--
-- Every PS-sourced table carries `source` (process_street|manual -- a
-- CHECK, not a Postgres ENUM type, matching this codebase's own
-- established convention of plain TEXT for closed sets added after the
-- original users migration's ENUM types), a `ps_*_run_id`
-- back-reference, a `raw_ps_snapshot JSONB` (the full field/task dump
-- from the last sync, so nothing not yet promoted to a named column is
-- silently discarded), and `last_synced_at`. Manual (non-PS) clients
-- stay supported -- `source = 'manual'` rows simply have null
-- PS-specific columns.
--
-- Values pulled from PS form fields are stored as raw text everywhere
-- in Facility Policies, deliberately not parsed into decimals/booleans
-- -- Boris's explicit call for Phase 1, since even PS's own most
-- structured step (Coverage) is meant to be read/copied by a human, not
-- computed against, for now.

CREATE SCHEMA clients;

CREATE TABLE clients.companies (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    legal_name TEXT NOT NULL,
    -- Null when the same as legal_name -- PS's own "Legal Name? Same/
    -- Different than Business DBA" pattern.
    dba_name TEXT,
    corporate_email TEXT,
    corporate_phone TEXT,
    corporate_address_street TEXT,
    corporate_address_city TEXT,
    corporate_address_state TEXT,
    corporate_address_zip TEXT,
    source TEXT NOT NULL CHECK (source IN ('process_street', 'manual')),
    -- Corporate fields live on whichever facility's Intake run answered
    -- "first time = Yes" -- this is that run's id, not a company-level
    -- PS object (PS has none).
    ps_intake_run_id TEXT,
    raw_ps_snapshot JSONB,
    last_synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER companies_set_updated_at
    BEFORE UPDATE ON clients.companies
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

CREATE TABLE clients.facilities (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    company_id UUID NOT NULL REFERENCES clients.companies(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    street_address TEXT,
    city TEXT,
    state TEXT,
    zip TEXT,
    phone TEXT,
    email TEXT,
    units_count INTEGER,
    primary_storage_offering TEXT,
    -- What PMS/system the facility used before this one -- PS's own
    -- "What Property Management Software is this facility currently
    -- using?" field.
    previous_pms TEXT,
    access_control_system TEXT,
    go_live_date DATE,
    -- Seeded from PS's own Dropbox-folder field but deliberately
    -- editable in OO afterward -- Boris's "customizable" requirement.
    dropbox_folder_url TEXT,
    source TEXT NOT NULL CHECK (source IN ('process_street', 'manual')),
    ps_intake_run_id TEXT,
    raw_ps_snapshot JSONB,
    last_synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX facilities_company_id_idx ON clients.facilities(company_id);

CREATE TRIGGER facilities_set_updated_at
    BEFORE UPDATE ON clients.facilities
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Facility Policies: owned 1:1 by exactly one facility (facility_id IS
-- the primary key, not a separate synthetic id) -- see this file's
-- header for why there is no shared/pointed-to row here.
CREATE TABLE clients.facility_policies (
    facility_id UUID PRIMARY KEY REFERENCES clients.facilities(id) ON DELETE CASCADE,
    raw_ps_snapshot JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER facility_policies_set_updated_at
    BEFORE UPDATE ON clients.facility_policies
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Fees sub-tab. One row per named fee -- real PS data shows variable
-- cardinality (Highway 20 alone had 9 named "other fees"), so this is a
-- list, not fixed nsf/admin/transfer/cleaning columns.
CREATE TABLE clients.policy_fees (
    id BIGSERIAL PRIMARY KEY,
    facility_policies_id UUID NOT NULL REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    -- The known-type tag. Most of PS's own fee fields are already
    -- separately labeled (NSF/Chargeback Fee, Move-In Admin Fee,
    -- Transfer Fee, Cleaning Fee, Security Deposit) -- 'other' is for
    -- anything that only ever existed inside PS's free-text "Any Other
    -- Fees" catch-all, kept as one raw_value per named item there, not
    -- split apart further.
    fee_type TEXT NOT NULL CHECK (
        fee_type IN ('security_deposit', 'nsf_chargeback', 'move_in_admin', 'transfer', 'cleaning', 'other')
    ),
    -- Verbatim PS field label -- load-bearing when fee_type = 'other',
    -- since "Any Other Fees" has no sub-label of its own.
    label TEXT,
    -- Verbatim PS value, e.g. "Optional Electrical $30.00 (R)" -- not
    -- decomposed. Displayed with a copy button in OO, not computed
    -- against.
    raw_value TEXT NOT NULL,
    -- Unpopulated for now -- see the recurring/notice-type parsing note
    -- in the vault schema doc. Real PS data shows at least three
    -- different phrasings for "recurring" ("(R)", "(YES Recurring)")
    -- and one for "one-time" ("(One-Time)") inside raw_value; nothing
    -- parses this yet.
    is_recurring BOOLEAN,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX policy_fees_facility_policies_id_idx ON clients.policy_fees(facility_policies_id);

-- Taxes sub-tab. More structured than Fees in PS itself (Sales vs. Rent
-- vs. Additional One-Time vs. Additional Recurring are distinct PS
-- fields), but every value here is still raw text, not parsed --
-- consistent with everything else in Facility Policies for Phase 1.
CREATE TABLE clients.policy_taxes (
    facility_policies_id UUID PRIMARY KEY REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    sales_tax_applies_raw TEXT,
    sales_tax_rate_raw TEXT,
    rent_tax_applies_raw TEXT,
    rent_tax_rate_raw TEXT,
    -- Can name specific unit sizes/types, not just yes/no (Boris's own
    -- flag) -- must stay free text, never a boolean.
    rent_tax_applies_to_all_units_raw TEXT,
    -- PS's "Additional One-Time Taxes - Name/Rate/Attribute Payable"
    -- field, verbatim.
    other_one_time_taxes_raw TEXT,
    -- PS's "Name(s) & Rate(s) of Add'l Recurring Taxes" field, verbatim.
    other_recurring_taxes_raw TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER policy_taxes_set_updated_at
    BEFORE UPDATE ON clients.policy_taxes
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Delinquency sub-tab. An ordered sequence -- notices, late fees,
-- pre-lien, lien, cut lock, auction, "possibly more" per Boris -- not a
-- fixed late_fee_1/2/3 shape. Flagged as needing real design work later
-- (editable free-form + dropdowns, recurring/notice-channel parsing);
-- this is the Phase 1 capture shape that work builds on top of.
CREATE TABLE clients.policy_delinquency_steps (
    id BIGSERIAL PRIMARY KEY,
    facility_policies_id UUID NOT NULL REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    step_order INTEGER NOT NULL,
    step_type TEXT NOT NULL CHECK (
        step_type IN ('late_fee', 'pre_lien', 'lien', 'cut_lock', 'auction', 'notice', 'other')
    ),
    -- Verbatim PS value, e.g. "Name this Lockout Fee $10.00 (One-Time)".
    raw_value TEXT NOT NULL,
    -- Both unpopulated for now -- notice channel currently only ever
    -- appears in free-form comments, no structured PS field for it.
    is_recurring BOOLEAN,
    notice_channel TEXT CHECK (
        notice_channel IS NULL OR notice_channel IN ('document', 'email', 'sms', 'combination', 'unknown')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (facility_policies_id, step_order)
);

CREATE INDEX policy_delinquency_steps_facility_policies_id_idx
    ON clients.policy_delinquency_steps(facility_policies_id);

-- Coverage sub-tab (tiers). Genuinely multi-row -- PS supports up to 6
-- tiers -- kept as raw text per Boris's explicit call, not decimals,
-- even though this is PS's most structured step.
CREATE TABLE clients.policy_coverage_tiers (
    id BIGSERIAL PRIMARY KEY,
    facility_policies_id UUID NOT NULL REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    tier_number INTEGER NOT NULL,
    total_coverage_amount_raw TEXT,
    cost_to_tenant_raw TEXT,
    UNIQUE (facility_policies_id, tier_number)
);

CREATE INDEX policy_coverage_tiers_facility_policies_id_idx
    ON clients.policy_coverage_tiers(facility_policies_id);

-- Coverage sub-tab (commission). Folded under Coverage, not its own tab
-- -- commission is earned off protection-plan sales. Stays 1:1, no
-- variable cardinality observed here the way Fees/Delinquency have.
CREATE TABLE clients.policy_commission (
    facility_policies_id UUID PRIMARY KEY REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    commission_type_raw TEXT,
    dollar_amount_raw TEXT,
    percent_amount_raw TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER policy_commission_set_updated_at
    BEFORE UPDATE ON clients.policy_commission
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Specials sub-tab. Deliberately one raw blob, not split into rows per
-- special -- PS itself stores the whole nested-bullet promo block as a
-- single field value, and the point is preserving it exactly as entered
-- for copy-paste into QMS, not computing against it.
CREATE TABLE clients.policy_specials (
    facility_policies_id UUID PRIMARY KEY REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    -- Whitespace/indentation preserved verbatim -- the copy button is
    -- the point.
    raw_text TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER policy_specials_set_updated_at
    BEFORE UPDATE ON clients.policy_specials
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- People: owners/district managers/managers/signers. Deduped globally
-- by email (CITEXT, matching auth.users' own case-insensitive email
-- convention) -- name+phone-only matches need the same manual-review
-- treatment the dedup tool already gives tenant contacts, not an
-- automated merge.
CREATE TABLE clients.people (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    full_name TEXT NOT NULL,
    email CITEXT,
    phone TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX people_email_idx ON clients.people(email) WHERE email IS NOT NULL;

CREATE TRIGGER people_set_updated_at
    BEFORE UPDATE ON clients.people
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Many-to-many at the facility grain -- matches how PS itself actually
-- captures Owner/District Manager/Manager (per facility, even when the
-- same person/text is copy-pasted across every sister facility). A
-- person who is both Owner and Signer simply gets two rows; no merge
-- happens at ingestion. No company-level table is needed: a district
-- manager who oversees three facilities is just the same person_id
-- appearing in three rows here.
CREATE TABLE clients.facility_people (
    facility_id UUID NOT NULL REFERENCES clients.facilities(id) ON DELETE CASCADE,
    person_id UUID NOT NULL REFERENCES clients.people(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (
        role IN ('owner', 'district_manager', 'manager', 'signer', 'order_placer', 'poc')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (facility_id, person_id, role)
);

CREATE INDEX facility_people_person_id_idx ON clients.facility_people(person_id);

-- Elavon tab. Confirmed always 1:1 with a facility (Boris).
CREATE TABLE clients.facility_merchant_accounts (
    facility_id UUID PRIMARY KEY REFERENCES clients.facilities(id) ON DELETE CASCADE,
    rate_provided TEXT,
    application_status TEXT,
    credentials_added_to_qms BOOLEAN NOT NULL DEFAULT false,
    source TEXT NOT NULL CHECK (source IN ('process_street', 'manual')),
    ps_new_merchant_run_id TEXT,
    raw_ps_snapshot JSONB,
    last_synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER facility_merchant_accounts_set_updated_at
    BEFORE UPDATE ON clients.facility_merchant_accounts
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Contract Order. Nullable existence per facility -- confirmed by
-- Boris this is inconsistent for territory/sales-process reasons, not a
-- data gap to chase. When present, migrating_from_system (the legacy
-- system named in the order) is the operationally important field --
-- the whole reason this workflow is worth having in OO at all.
CREATE TABLE clients.facility_contract_orders (
    facility_id UUID PRIMARY KEY REFERENCES clients.facilities(id) ON DELETE CASCADE,
    migrating_from_system TEXT,
    source TEXT NOT NULL CHECK (source IN ('process_street', 'manual')),
    ps_contract_order_run_id TEXT,
    raw_ps_snapshot JSONB,
    last_synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER facility_contract_orders_set_updated_at
    BEFORE UPDATE ON clients.facility_contract_orders
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

-- Generic step-completion tracking across all three PS workflows --
-- avoids hardcoding a column per PS step name, which would break the
-- moment PS's own template steps change. Answers "is this step done in
-- PS" for the OO UI. UNIQUE(facility_id, workflow, ps_task_id) is the
-- upsert target a re-sync writes against.
CREATE TABLE clients.ps_task_status (
    id BIGSERIAL PRIMARY KEY,
    facility_id UUID NOT NULL REFERENCES clients.facilities(id) ON DELETE CASCADE,
    workflow TEXT NOT NULL CHECK (workflow IN ('intake', 'merchant_account', 'contract_order')),
    ps_task_id TEXT NOT NULL,
    task_name TEXT NOT NULL,
    -- Plain text, deliberately unconstrained -- these are PS's own
    -- status strings ("Completed"/"NotCompleted" observed so far), not
    -- a set this codebase controls, so a CHECK here would just be a
    -- future outage waiting for PS to add a value like "Skipped".
    status TEXT NOT NULL,
    last_synced_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (facility_id, workflow, ps_task_id)
);

CREATE INDEX ps_task_status_facility_id_idx ON clients.ps_task_status(facility_id);

-- Row Level Security -- read: any authenticated caller (matches the
-- `sales` role's "read-only access to client records" description).
-- Write: onboarding_manager or department_manager only, the two roles
-- actually holding client_ops.perform (see
-- 20260806120000_add_roles_permissions_tables) -- reused here rather
-- than a new permission key, since the capability being gated (perform
-- client operations) hasn't changed, only which schema hosts the data.
-- Admin deliberately does not get write access, same posture
-- client_ops.audit_log's own migration already established ("admin ...
-- never performs or approves client operations").
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'companies', 'facilities', 'facility_policies', 'policy_fees', 'policy_taxes',
        'policy_delinquency_steps', 'policy_coverage_tiers', 'policy_commission',
        'policy_specials', 'people', 'facility_people', 'facility_merchant_accounts',
        'facility_contract_orders', 'ps_task_status'
    ]
    LOOP
        EXECUTE format('ALTER TABLE clients.%I ENABLE ROW LEVEL SECURITY', t);

        EXECUTE format(
            'CREATE POLICY %I ON clients.%I FOR SELECT
                 USING (NULLIF(current_setting(''app.current_user_id'', true), '''') IS NOT NULL)',
            t || '_select_authenticated', t
        );

        EXECUTE format(
            'CREATE POLICY %I ON clients.%I FOR INSERT
                 WITH CHECK (
                     auth.current_user_has_role(''onboarding_manager'')
                     OR auth.current_user_has_role(''department_manager'')
                 )',
            t || '_insert_client_ops_roles', t
        );
        EXECUTE format(
            'CREATE POLICY %I ON clients.%I FOR UPDATE
                 USING (
                     auth.current_user_has_role(''onboarding_manager'')
                     OR auth.current_user_has_role(''department_manager'')
                 )
                 WITH CHECK (
                     auth.current_user_has_role(''onboarding_manager'')
                     OR auth.current_user_has_role(''department_manager'')
                 )',
            t || '_update_client_ops_roles', t
        );
        EXECUTE format(
            'CREATE POLICY %I ON clients.%I FOR DELETE
                 USING (
                     auth.current_user_has_role(''onboarding_manager'')
                     OR auth.current_user_has_role(''department_manager'')
                 )',
            t || '_delete_client_ops_roles', t
        );
    END LOOP;
END
$$;
