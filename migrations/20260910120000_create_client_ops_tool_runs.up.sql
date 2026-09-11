-- client_ops.tool_runs: a durable, per-facility record of a tool run
-- (starting with Dedup only -- `tool` will grow a 'unit_groups'/
-- 'template_tagger' CHECK value once those tools get wired up the same
-- way, not before). Distinct from client_ops.audit_log (a generic,
-- facility-blind append-only event trail that already logs a
-- DEDUP_COMPLETED event on export) -- this table is a real, queryable
-- row per run, joined by the new Onboarding Work tab on the facility
-- page, carrying the actual report body and output location, not just
-- an event note.
--
-- facility_id is NOT NULL -- deliberately stricter than today's loose,
-- audit-only `client_id: Option<Uuid>` on the dedup export endpoints.
-- The whole point of this feature is that a run can't exist unrelated
-- to a facility.
--
-- One table, not a separate output-blob table: one output file per run,
-- KB-scale, mutually exclusive with a Dropbox path -- normalizing that
-- into its own table would only add a join for no benefit.
CREATE TABLE client_ops.tool_runs (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    tool TEXT NOT NULL CHECK (tool IN ('dedup')),
    facility_id UUID NOT NULL REFERENCES clients.facilities(id) ON DELETE CASCADE,

    -- The in-memory session id (SessionMetadata.id) -- TEXT, matching
    -- the Rust `String` type used end-to-end for session ids, so
    -- export's later lookup needs no parse step. This is how export
    -- finds this row again to attach output columns.
    session_id TEXT NOT NULL,

    actor_user_id UUID REFERENCES auth.users(id) ON DELETE SET NULL,

    source_file_name TEXT NOT NULL,
    -- Full path to the source file in Dropbox -- NULL for a
    -- locally-uploaded file, which has nothing to link to.
    source_dropbox_path TEXT,

    -- DedupReportView, verbatim -- already the exact shape the
    -- collapsed-results UI consumes, small (aggregated groups/
    -- candidates, not per-row tenant records), and schema-free enough
    -- that a later tool's differently-shaped report can reuse this same
    -- column without a migration.
    report_summary JSONB NOT NULL,

    -- Output, filled in later by export/export_to_dropbox -- NULL until
    -- (if ever) the user exports. Mutually exclusive: a run's output is
    -- either an in-DB file (browser-download case) or a Dropbox path,
    -- never both -- whichever export a user triggers last wins, same as
    -- Dropbox's own upload-overwrite semantics.
    output_bytes BYTEA,
    output_content_type TEXT,
    output_file_name TEXT,
    output_dropbox_path TEXT,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,

    CONSTRAINT tool_runs_output_bytes_fields_together CHECK (
        (output_bytes IS NULL) = (output_content_type IS NULL)
        AND (output_bytes IS NULL) = (output_file_name IS NULL)
    ),
    CONSTRAINT tool_runs_output_mutually_exclusive CHECK (
        NOT (output_bytes IS NOT NULL AND output_dropbox_path IS NOT NULL)
    )
);

-- The Onboarding Work tab's own list query: newest-first per facility,
-- filtered by tool.
CREATE INDEX tool_runs_facility_tool_created_idx
    ON client_ops.tool_runs(facility_id, tool, created_at DESC);

-- What export looks the row back up by.
CREATE UNIQUE INDEX tool_runs_session_id_idx ON client_ops.tool_runs(session_id);

ALTER TABLE client_ops.tool_runs ENABLE ROW LEVEL SECURITY;

-- INSERT/UPDATE unconditional (app-gated) -- same reasoning as
-- client_ops.audit_log's own insert policy: only an authenticated
-- caller who already reached dedup's handlers (no stricter permission
-- gate than that today) ever writes here, and blocking a write on
-- identity context risks turning a persistence hiccup into a broken
-- tool run. UPDATE gets the same treatment as INSERT because the only
-- two writers (export/export_to_dropbox) already enforce session
-- ownership via the in-memory SessionStore before ever reaching this
-- query -- RLS re-checking that here would just duplicate a check
-- already made.
CREATE POLICY tool_runs_insert_unconditional ON client_ops.tool_runs
    FOR INSERT WITH CHECK (true);

CREATE POLICY tool_runs_update_unconditional ON client_ops.tool_runs
    FOR UPDATE USING (true) WITH CHECK (true);

-- Any authenticated caller may read -- same posture as clients.*
-- facility-scoped tables (facility_people etc.), since this table backs
-- one tab among many on the facility page and the others aren't
-- role-restricted either.
CREATE POLICY tool_runs_select_authenticated ON client_ops.tool_runs
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

-- No DELETE policy -- RLS default-denies it, same append-only posture
-- as client_ops.audit_log.
