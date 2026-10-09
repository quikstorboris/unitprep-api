DELETE FROM integrations.clickup_task_steps WHERE tool <> 'dedup';

ALTER TABLE integrations.clickup_task_steps
    DROP CONSTRAINT clickup_task_steps_tool_ordinal_unique,
    DROP CONSTRAINT clickup_task_steps_comment_text_not_blank,
    DROP COLUMN comment_only_from_sequence,
    DROP COLUMN comment_without_link,
    DROP COLUMN comment_link_text,
    DROP COLUMN comment_lead,
    DROP COLUMN tool;
