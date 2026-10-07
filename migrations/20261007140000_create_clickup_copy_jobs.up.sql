-- Progress and results of a ClickUp Copy too big to finish inside the
-- request that started it (see `api::clickup_copy`'s bulk copy): one
-- comment copied to many facilities' lists, slowed to ClickUp's rate
-- limit, run by a background task while the person carries on and is
-- told when it is done.
--
-- This is the only state ClickUp Copy keeps. What was copied is not
-- recorded per task -- ClickUp is the record -- but a job that outlives
-- its request has to be findable by the page that polls it, and has to
-- say what happened row by row when it finishes.
--
-- A job is owned by the person who started it (their ClickUp token does
-- the work), and only they can see or touch it. `updated_at` is bumped as
-- each batch of rows completes; a job still `running` whose `updated_at`
-- has gone quiet (the server restarted mid-copy) is reported as
-- interrupted when read, rather than swept at startup: a startup sweep
-- would have to read every user's rows, which the own-row policies below
-- rightly forbid.
CREATE TABLE client_ops.clickup_copy_jobs (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    company_id UUID NOT NULL REFERENCES clients.companies(id) ON DELETE CASCADE,
    created_by UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    -- Snapshots, not foreign keys: shown on the page's job list even if the
    -- facility or ClickUp task is later renamed or gone.
    source_facility_id UUID NOT NULL,
    source_task_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'done', 'failed')),
    total INTEGER NOT NULL CHECK (total > 0),
    copied INTEGER NOT NULL DEFAULT 0 CHECK (copied >= 0),
    failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
    -- One entry per destination row finished so far.
    results JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- Why the whole job failed, when it did.
    message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE INDEX clickup_copy_jobs_owner_idx
    ON client_ops.clickup_copy_jobs (created_by, created_at DESC);

ALTER TABLE client_ops.clickup_copy_jobs ENABLE ROW LEVEL SECURITY;

CREATE POLICY clickup_copy_jobs_select_own ON client_ops.clickup_copy_jobs FOR SELECT
    USING (created_by = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);

CREATE POLICY clickup_copy_jobs_insert_own ON client_ops.clickup_copy_jobs FOR INSERT
    WITH CHECK (created_by = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);

CREATE POLICY clickup_copy_jobs_update_own ON client_ops.clickup_copy_jobs FOR UPDATE
    USING (created_by = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid)
    WITH CHECK (created_by = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);
