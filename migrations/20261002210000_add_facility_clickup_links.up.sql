-- Links a facility to the ClickUp list that tracks its onboarding.
--
-- Stored as plain columns on clients.facilities (like dropbox_folder_url)
-- rather than a side table: it is a 0-or-1 relationship per facility, it
-- is read on every facility/company page, and nothing else keys off it.
-- Process Street re-syncs never touch these columns (they are not
-- Process-Street-sourced).
--
-- Only the list's stable ID is authoritative; name, folder name and URL
-- are a snapshot taken at link time so the pages can show and open the
-- link without a ClickUp round trip (and still say something sensible if
-- the list is later renamed or deleted in ClickUp). Several facilities
-- MAY point at one list (no unique index): the live data suggests
-- one-list-per-facility is the norm, but a company-wide list shared by a
-- couple of its facilities is plausible, and the UI warns rather than
-- blocks.
--
-- Writes go through the existing facilities UPDATE policy (client-ops
-- roles); reads through the existing authenticated SELECT policy.
ALTER TABLE clients.facilities
    ADD COLUMN clickup_list_id TEXT,
    ADD COLUMN clickup_list_name TEXT,
    ADD COLUMN clickup_folder_name TEXT,
    ADD COLUMN clickup_list_url TEXT,
    ADD COLUMN clickup_linked_by UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    ADD COLUMN clickup_linked_at TIMESTAMPTZ,
    ADD CONSTRAINT facilities_clickup_link_all_or_nothing CHECK (
        (clickup_list_id IS NULL) = (clickup_list_name IS NULL)
        AND (clickup_list_id IS NULL) = (clickup_list_url IS NULL)
        AND (clickup_list_id IS NULL) = (clickup_linked_at IS NULL)
    );

CREATE INDEX idx_facilities_clickup_list_id
    ON clients.facilities (clickup_list_id)
    WHERE clickup_list_id IS NOT NULL;

-- Non-secret ClickUp settings shared by everyone. Today just which space
-- holds the onboarding lists, held as data (looked up by NAME at runtime)
-- rather than a hardcoded ClickUp id: ids are an implementation detail of
-- one workspace, the name is what a person would recognize and edit.
-- Singleton row, same shape as integrations.dropbox_configuration.
CREATE TABLE integrations.clickup_settings (
    id SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    onboarding_space_name TEXT NOT NULL DEFAULT 'QMS Onboarding' CHECK (btrim(onboarding_space_name) <> ''),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES auth.users(id) ON DELETE SET NULL
);

CREATE TRIGGER clickup_settings_set_updated_at
    BEFORE UPDATE ON integrations.clickup_settings
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

INSERT INTO integrations.clickup_settings (id) VALUES (1);

ALTER TABLE integrations.clickup_settings ENABLE ROW LEVEL SECURITY;

-- Not a secret, and every ClickUp user needs it to link, so any
-- authenticated caller may read it; only admins/developers may change it
-- (matching the other integration settings tables).
CREATE POLICY clickup_settings_select_authenticated
    ON integrations.clickup_settings FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY clickup_settings_update_admin_or_developer
    ON integrations.clickup_settings FOR UPDATE
    USING (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'))
    WITH CHECK (auth.current_user_has_role('admin') OR auth.current_user_has_role('developer'));
