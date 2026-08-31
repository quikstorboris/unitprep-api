-- Person-name search, Phase 2's harder half. PS has no server-side
-- search over form-field values (only over a run's own title, see
-- `ProcessStreetClient::search_workflow_runs_by_name`), so finding a
-- facility by an owner/DM/manager/signer/POC's name needs its own
-- lightweight, locally-searchable index -- these two tables are that
-- index's storage, not a general-purpose PS mirror.
--
-- Deliberately separate from `clients.people`/`clients.facilities`,
-- which represent real, deliberately-imported client records (created
-- only via the "Add to OO" action, still Phase 3). `ps_person_index`
-- holds a thin, disposable projection (name/email/phone/role) of every
-- run PS has, whether or not anyone has ever imported it -- rebuilt
-- wholesale per run on every sync, never hand-edited, and safe to
-- truncate and rebuild from scratch at any time. No foreign key to
-- `clients.facilities` for the same reason: the facility this run
-- describes may not exist in `clients` at all yet.
--
-- `ps_sync_state` is the delta-tracking half: one row per
-- (workflow, ps_run_id), holding PS's own `audit.updatedDate` as of the
-- last time this run's fields were actually fetched. The background
-- sync (see `clients::sync`) compares this against the run's *current*
-- `updatedDate` (available cheaply from the list-runs call, no
-- form-fields fetch needed) and only re-fetches+re-indexes a run whose
-- timestamp has moved -- the actual "delta" saving, since `updatedDate`
-- only changes when someone edits that specific run in PS.
CREATE TABLE clients.ps_sync_state (
    workflow TEXT NOT NULL CHECK (workflow IN ('intake', 'merchant_account', 'contract_order')),
    ps_run_id TEXT NOT NULL,
    run_name TEXT NOT NULL,
    -- PS's own `audit.updatedDate` as of the last successful index of
    -- this run -- not this row's own `now()`, which would defeat the
    -- comparison this table exists for.
    ps_updated_at TIMESTAMPTZ NOT NULL,
    last_synced_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workflow, ps_run_id)
);

-- Rebuilt wholesale (delete-then-insert, keyed on workflow+ps_run_id)
-- every time a run's ps_sync_state entry advances -- never updated
-- row-by-row, so no updated_at trigger.
CREATE TABLE clients.ps_person_index (
    id BIGSERIAL PRIMARY KEY,
    workflow TEXT NOT NULL CHECK (workflow IN ('intake', 'merchant_account', 'contract_order')),
    ps_run_id TEXT NOT NULL,
    run_name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    email TEXT,
    phone TEXT,
    role TEXT NOT NULL CHECK (
        role IN ('owner', 'district_manager', 'manager', 'signer', 'onboarding_poc', 'website_poc', 'integration_poc')
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ps_person_index_run_idx ON clients.ps_person_index(workflow, ps_run_id);
-- Case-insensitive prefix/equality search -- a full substring index
-- (pg_trgm) isn't worth the new extension yet at this data's real
-- scale (low thousands of rows at most); revisit if a plain ILIKE scan
-- ever shows up slow.
CREATE INDEX ps_person_index_full_name_idx ON clients.ps_person_index(lower(full_name));
CREATE INDEX ps_person_index_email_idx ON clients.ps_person_index(lower(email)) WHERE email IS NOT NULL;

-- RLS: same posture as every other `clients` table (see
-- 20260828120000's own comment) -- broad authenticated read (this is
-- just names/emails/phones, the same sensitivity tier as
-- `clients.people`), writes gated to onboarding_manager/
-- department_manager. In practice only the background sync task ever
-- writes here (using that same role pair via its RLS transaction, see
-- `clients::sync`'s `SYSTEM_USER_ID`) -- no separate "system" role
-- exists in this app's RBAC, so reusing the established client-ops
-- write gate is the pragmatic choice over inventing a new one for a
-- single caller.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['ps_sync_state', 'ps_person_index']
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
