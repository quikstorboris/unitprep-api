ALTER TABLE client_ops.tool_runs
    DROP CONSTRAINT IF EXISTS tool_runs_source_bytes_fields_together;

ALTER TABLE client_ops.tool_runs
    DROP COLUMN IF EXISTS source_bytes,
    DROP COLUMN IF EXISTS source_content_type;
