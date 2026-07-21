DROP POLICY IF EXISTS totp_credentials_owner_only ON totp_credentials;
ALTER TABLE totp_credentials DISABLE ROW LEVEL SECURITY;
