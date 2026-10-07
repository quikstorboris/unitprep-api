-- ClickUp Copy, Phase 1: which facility's ClickUp list is the company's
-- "parent" (the source its comments are copied from), and whether the
-- company was deliberately created without a ClickUp project.
--
-- The parent is a plain nullable FK on the company, like the facility's
-- own ClickUp link: 0-or-1 per company, read on every company page. ON
-- DELETE SET NULL so removing a facility never blocks on, or dangles from,
-- the designation. Whether the designated facility actually has a linked
-- list is enforced by the write endpoint, not here: the link can be
-- removed later and the designation should then simply show as "needs a
-- list", not be silently erased.
--
-- The waiver is the "Create without ClickUp project" checkbox on the
-- Review & Create screen. Keeping *who/when* distinguishes a deliberate
-- "no ClickUp for this client" from "nobody linked it yet", which is the
-- difference the company page's warning needs.
--
-- Parent designations are rare but worth auditing, so every change
-- (including the very first designation) is appended to a history table;
-- the Copy Comments section reads it chronologically.
ALTER TABLE clients.companies
    ADD COLUMN clickup_parent_facility_id UUID REFERENCES clients.facilities(id) ON DELETE SET NULL,
    ADD COLUMN clickup_waived_at TIMESTAMPTZ NULL,
    -- No all-or-nothing CHECK against the timestamp: ON DELETE SET NULL
    -- legitimately clears this alone when the user row is removed.
    ADD COLUMN clickup_waived_by UUID NULL REFERENCES auth.users(id) ON DELETE SET NULL;

CREATE TABLE clients.company_clickup_parent_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    company_id UUID NOT NULL REFERENCES clients.companies(id) ON DELETE CASCADE,
    -- Snapshots, not FKs to facilities: a facility may later be deleted
    -- and the history should still say which one it was.
    from_facility_id UUID NULL,
    from_facility_name TEXT NULL,
    to_facility_id UUID NULL,
    to_facility_name TEXT NULL,
    changed_by UUID NULL REFERENCES auth.users(id) ON DELETE SET NULL,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX company_clickup_parent_history_company_idx
    ON clients.company_clickup_parent_history (company_id, changed_at);

ALTER TABLE clients.company_clickup_parent_history ENABLE ROW LEVEL SECURITY;

-- Append-only: no UPDATE or DELETE policy, so neither is possible for the
-- application role. Readable by any authenticated caller (it is
-- navigation context, like the facility's own link); writable by the
-- client-ops roles that may designate a parent.
CREATE POLICY company_clickup_parent_history_select_authenticated
    ON clients.company_clickup_parent_history FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY company_clickup_parent_history_insert_client_ops_roles
    ON clients.company_clickup_parent_history FOR INSERT
    WITH CHECK (auth.current_user_is_client_ops_role());
