-- The onboarding steps Orchestrator can find and update in a facility's
-- ClickUp list, and the task-name phrases that identify each one.
--
-- Held as data, not Rust constants: template wording changes ("PERFORM"
-- vs "COMPLETE", renamed tasks) and someone other than an engineer should
-- be able to add a phrase without a deploy. Matching is fuzzy (see
-- `clickup::task_matching`): these phrases are compared by their words,
-- ignoring verbs, numbering, emoji and plurals, and the person always
-- confirms the task before anything is written.
--
-- Today only the duplicate-check steps exist. `ordinal` is which check the
-- step stands for (1 = first, 2 = second and any later one) and is what
-- keeps a "2nd Duplicate Check" task from being offered for the first.
CREATE TABLE integrations.clickup_task_steps (
    step_key TEXT PRIMARY KEY,
    label TEXT NOT NULL CHECK (btrim(label) <> ''),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
    phrases TEXT[] NOT NULL CHECK (cardinality(phrases) > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES auth.users(id) ON DELETE SET NULL
);

CREATE TRIGGER clickup_task_steps_set_updated_at
    BEFORE UPDATE ON integrations.clickup_task_steps
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

INSERT INTO integrations.clickup_task_steps (step_key, label, ordinal, phrases) VALUES
    ('dedup_first', '1st Duplicate Check', 1,
     ARRAY['COMPLETE Duplicate Tenant Corrections', 'PERFORM Duplicate Check', 'PERFORM 1st Duplicate Check', 'Evaluate Tenant Contact List for Duplicates']),
    ('dedup_second', '2nd Duplicate Check', 2,
     ARRAY['PERFORM 2nd Duplicate Check', 'Evaluate Tenant Contact List for Duplicates']);

ALTER TABLE integrations.clickup_task_steps ENABLE ROW LEVEL SECURITY;

-- Not secret and every ClickUp user needs it, so any authenticated caller
-- may read; only admins/developers may edit (same as clickup_settings).
CREATE POLICY clickup_task_steps_select_authenticated
    ON integrations.clickup_task_steps FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY clickup_task_steps_update_admin_or_developer
    ON integrations.clickup_task_steps FOR UPDATE
    USING (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'))
    WITH CHECK (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'));
