-- Redesigns Taxes and Delinquency from free-text into real structured
-- data (Boris, 2026-09-08). Both old tables (clients.policy_taxes,
-- clients.policy_delinquency_steps) are left untouched, NOT dropped or
-- migrated automatically -- real historical free-text data exists on
-- Highway 20 Self Storage for both (confirmed live: 1 real tax row, 9
-- real delinquency rows), and turning that prose into the new
-- structured fields (a real dollar amount, a real trigger reference)
-- needs a human's judgment call, not a parser -- one delinquency row
-- alone ("Certificate of Mailing $5.00" + "Auction Advertising Fee
-- $55.00" in the same free-text field) would need to become two
-- separate structured rows. The old tables stay queryable as a
-- historical read-only reference; the new tabs work off these new
-- tables going forward.

-- Taxes: was a single 1:1 row of 7 raw-text fields; is now a list, one
-- row per tax, matching Fees' own existing shape. `tax_type` is a
-- forward-compatible discriminator -- only 'fixed' is actually
-- supported by the API/UI yet ("Marginal" and "Percentage" are planned
-- but explicitly deferred, each will likely need its own extra fields
-- once designed).
CREATE TABLE clients.policy_tax_entries (
    id BIGSERIAL PRIMARY KEY,
    facility_policies_id UUID NOT NULL REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    tax_type TEXT NOT NULL DEFAULT 'fixed' CHECK (tax_type IN ('fixed', 'marginal', 'percentage')),
    tax_name TEXT NOT NULL CHECK (tax_name IN ('sales', 'rental')),
    description TEXT,
    flat_amount NUMERIC,
    attribute_payable_percent NUMERIC,
    is_recurring BOOLEAN NOT NULL DEFAULT false,
    sort_order INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER policy_tax_entries_set_updated_at
    BEFORE UPDATE ON clients.policy_tax_entries
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

CREATE INDEX policy_tax_entries_facility_policies_id_idx
    ON clients.policy_tax_entries(facility_policies_id);

-- Delinquency: was step_order/step_type/raw_value; now a real dollar
-- amount (required, 0 acceptable) and a real trigger -- either the
-- facility's own Paid Through Date, or another step already configured
-- on this same schedule, referenced by category (Boris's own call,
-- 2026-09-08: category rather than a specific row id, since a
-- facility's schedule can't reasonably have two rows in the same
-- category -- simpler to build and edit, survives reordering).
CREATE TABLE clients.policy_delinquency_entries (
    id BIGSERIAL PRIMARY KEY,
    facility_policies_id UUID NOT NULL REFERENCES clients.facility_policies(facility_id) ON DELETE CASCADE,
    category TEXT NOT NULL CHECK (
        category IN ('late_fee', 'pre_lien', 'lien', 'cut_lock', 'auction', 'notice', 'other')
    ),
    name TEXT NOT NULL,
    amount NUMERIC NOT NULL,
    days_after INTEGER,
    trigger_type TEXT NOT NULL DEFAULT 'paid_through_date' CHECK (
        trigger_type IN ('paid_through_date', 'step_category')
    ),
    -- Set only when trigger_type = 'step_category' -- e.g. Lien trigger
    -- off Pre-Lien rather than off Paid Through Date directly, the
    -- exact scenario Boris described (everything up to Lien keys off
    -- PTD, Lien itself keys off Pre-Lien).
    trigger_category TEXT CHECK (
        trigger_category IS NULL
        OR trigger_category IN ('late_fee', 'pre_lien', 'lien', 'cut_lock', 'auction', 'notice', 'other')
    ),
    sort_order INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (trigger_type = 'paid_through_date' AND trigger_category IS NULL)
        OR (trigger_type = 'step_category' AND trigger_category IS NOT NULL)
    )
);

CREATE TRIGGER policy_delinquency_entries_set_updated_at
    BEFORE UPDATE ON clients.policy_delinquency_entries
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

CREATE INDEX policy_delinquency_entries_facility_policies_id_idx
    ON clients.policy_delinquency_entries(facility_policies_id);

-- Same RLS shape as every other clients.* table: authenticated-only
-- read, onboarding_manager/department_manager write.
ALTER TABLE clients.policy_tax_entries ENABLE ROW LEVEL SECURITY;
ALTER TABLE clients.policy_delinquency_entries ENABLE ROW LEVEL SECURITY;

DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['policy_tax_entries', 'policy_delinquency_entries']
    LOOP
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
