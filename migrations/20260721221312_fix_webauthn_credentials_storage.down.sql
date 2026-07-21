ALTER TABLE webauthn_credentials
    DROP COLUMN passkey_data,
    ADD COLUMN public_key BYTEA NOT NULL DEFAULT '\x',
    ADD COLUMN sign_count BIGINT NOT NULL DEFAULT 0;

ALTER TABLE webauthn_credentials
    ALTER COLUMN public_key DROP DEFAULT;
