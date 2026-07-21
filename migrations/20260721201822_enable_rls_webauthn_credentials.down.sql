DROP POLICY IF EXISTS webauthn_credentials_owner_only ON webauthn_credentials;
ALTER TABLE webauthn_credentials DISABLE ROW LEVEL SECURITY;
