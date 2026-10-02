-- One ClickUp personal API token per Orchestrator user.
--
-- Deliberately not a singleton like integrations.dropbox_configuration:
-- ClickUp work has to be recorded against the person who triggered it, so
-- each user connects their own token and every ClickUp call is made with
-- the acting user's own credential. A shared admin token would attribute
-- every action to whoever owns it.
--
-- The token is stored only as ChaCha20-Poly1305 ciphertext
-- (`integrations::secrets`, AAD bound to the owning user id so a blob can
-- never be copied between users' rows). It is never returned to the
-- browser. `clickup_user_id`/`clickup_username` are what ClickUp itself
-- reported for the token at validation time, shown in the UI as
-- "connected as ...".
--
-- RLS: strictly the owner's own row, for every operation -- not even an
-- admin can read someone else's row. Whether a user may use ClickUp at all
-- is the `integrations.clickup` permission (app layer); this table only
-- guarantees isolation between users.
CREATE TABLE integrations.user_clickup_credentials (
    user_id UUID PRIMARY KEY REFERENCES auth.users(id) ON DELETE CASCADE,
    token_ciphertext BYTEA NOT NULL,
    clickup_user_id TEXT,
    clickup_username TEXT,
    status TEXT NOT NULL DEFAULT 'valid' CHECK (status IN ('valid', 'invalid')),
    last_validated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER user_clickup_credentials_set_updated_at
    BEFORE UPDATE ON integrations.user_clickup_credentials
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

ALTER TABLE integrations.user_clickup_credentials ENABLE ROW LEVEL SECURITY;

CREATE POLICY user_clickup_credentials_select_own ON integrations.user_clickup_credentials FOR SELECT
    USING (user_id = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);

CREATE POLICY user_clickup_credentials_insert_own ON integrations.user_clickup_credentials FOR INSERT
    WITH CHECK (user_id = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);

CREATE POLICY user_clickup_credentials_update_own ON integrations.user_clickup_credentials FOR UPDATE
    USING (user_id = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid)
    WITH CHECK (user_id = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);

CREATE POLICY user_clickup_credentials_delete_own ON integrations.user_clickup_credentials FOR DELETE
    USING (user_id = (NULLIF(current_setting('app.current_user_id', true), ''))::uuid);
