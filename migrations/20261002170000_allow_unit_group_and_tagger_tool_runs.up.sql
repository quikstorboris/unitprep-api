-- Onboarding Work records every tool run, not only Dedup: the Unit Group
-- (Group Prep) analysis and the Template Tagger check now get a row too,
-- under their own `tool` value, so the facility's Onboarding Work page can
-- list each activity on its own tab, newest first.
ALTER TABLE client_ops.tool_runs
    DROP CONSTRAINT IF EXISTS tool_runs_tool_check;
ALTER TABLE client_ops.tool_runs
    ADD CONSTRAINT tool_runs_tool_check
    CHECK (tool IN ('dedup', 'unit_group', 'tagger'));
