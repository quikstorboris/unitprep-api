ALTER TABLE totp_credentials ENABLE ROW LEVEL SECURITY;

CREATE POLICY totp_credentials_owner_only ON totp_credentials
    FOR ALL
    USING (user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid)
    WITH CHECK (user_id = NULLIF(current_setting('app.current_user_id', true), '')::uuid);
