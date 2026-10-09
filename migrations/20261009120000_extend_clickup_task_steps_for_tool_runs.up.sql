-- ClickUp task steps now cover every tool, not only duplicate checks, and
-- carry the wording of the comment each posts.
--
-- Held as data for the same reason the task-name phrases are (see
-- 20261005130000): the template wording and the comment text change
-- without a deploy, and a settings screen can edit them later. Nothing
-- about a tool or its task is a Rust constant any more.
--
-- `tool` is the client_ops.tool_runs.tool the step belongs to. A run is
-- matched to the step with the highest `ordinal` not above its position
-- among that facility's runs of the tool, so duplicate checks keep their
-- 1st/2nd steps and a tool with one step uses it for every run.
-- `comment_only_from_sequence` is the run position from which an update
-- only adds a comment (the task is already assigned and complete).
ALTER TABLE integrations.clickup_task_steps
    ADD COLUMN tool TEXT NOT NULL DEFAULT 'dedup'
        CHECK (tool IN ('dedup', 'unit_group', 'tagger')),
    ADD COLUMN comment_lead TEXT,
    ADD COLUMN comment_link_text TEXT,
    ADD COLUMN comment_without_link TEXT,
    ADD COLUMN comment_only_from_sequence INTEGER NOT NULL DEFAULT 2
        CHECK (comment_only_from_sequence >= 1);

UPDATE integrations.clickup_task_steps
   SET comment_lead = 'Duplicate check results are ',
       comment_link_text = 'here',
       comment_without_link = 'Duplicate check complete.',
       comment_only_from_sequence = 3
 WHERE tool = 'dedup';

INSERT INTO integrations.clickup_task_steps
    (step_key, label, ordinal, phrases, tool, comment_lead, comment_link_text, comment_without_link)
VALUES
    ('unit_groups', 'Unit Groups', 1, ARRAY['CONFIGURE Unit Setup'],
     'unit_group', 'Unit group results are ', 'here', 'Unit groups complete.'),
    ('template_tagger', 'Template Tagger', 1, ARRAY['APPLY TAGS to Lease'],
     'tagger', 'Template tagger results are ', 'here', 'Template tagger complete.');

ALTER TABLE integrations.clickup_task_steps
    ALTER COLUMN comment_lead SET NOT NULL,
    ALTER COLUMN comment_link_text SET NOT NULL,
    ALTER COLUMN comment_without_link SET NOT NULL,
    ADD CONSTRAINT clickup_task_steps_comment_text_not_blank CHECK (
        btrim(comment_lead) <> '' AND btrim(comment_link_text) <> '' AND btrim(comment_without_link) <> ''
    ),
    ADD CONSTRAINT clickup_task_steps_tool_ordinal_unique UNIQUE (tool, ordinal);
