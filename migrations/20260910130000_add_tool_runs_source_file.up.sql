-- Stores the original source file's own bytes on client_ops.tool_runs,
-- alongside (not instead of) source_dropbox_path -- a Dropbox-sourced
-- file can later be moved, renamed, or deleted out from under that
-- path, so this is the one copy Onboarding Work can always still hand
-- back regardless of what happens in Dropbox afterward. Nullable: the
-- one dev-test row that predates this column has neither.
ALTER TABLE client_ops.tool_runs
    ADD COLUMN source_bytes BYTEA,
    ADD COLUMN source_content_type TEXT;

ALTER TABLE client_ops.tool_runs
    ADD CONSTRAINT tool_runs_source_bytes_fields_together CHECK (
        (source_bytes IS NULL) = (source_content_type IS NULL)
    );
