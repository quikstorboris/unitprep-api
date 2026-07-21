ALTER TABLE webauthn_credentials
    DROP COLUMN public_key,
    DROP COLUMN sign_count,
    ADD COLUMN passkey_data JSONB NOT NULL DEFAULT '{}';

ALTER TABLE webauthn_credentials
    ALTER COLUMN passkey_data DROP DEFAULT;
