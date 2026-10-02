DELETE FROM client_ops.tool_runs WHERE tool IN ('unit_group', 'tagger');

ALTER TABLE client_ops.tool_runs
    DROP CONSTRAINT IF EXISTS tool_runs_tool_check;
ALTER TABLE client_ops.tool_runs
    ADD CONSTRAINT tool_runs_tool_check CHECK (tool = 'dedup');
