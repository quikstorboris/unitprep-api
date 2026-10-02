-- Restoring the constraint requires no run to hold both; where one does, keep
-- the in-database copy and drop the Dropbox path.
UPDATE client_ops.tool_runs
   SET output_dropbox_path = NULL
 WHERE output_bytes IS NOT NULL AND output_dropbox_path IS NOT NULL;

ALTER TABLE client_ops.tool_runs
    ADD CONSTRAINT tool_runs_output_mutually_exclusive CHECK (
        NOT (output_bytes IS NOT NULL AND output_dropbox_path IS NOT NULL)
    );
