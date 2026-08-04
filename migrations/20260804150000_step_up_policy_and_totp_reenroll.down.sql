-- Reversing this first would fail if any row currently has a NULL
-- secret_encrypted (an in-progress first enrolment, never confirmed) --
-- acceptable for a down-migration, same as any other down-migration that
-- assumes a clean/compatible data state to reverse into.
ALTER TABLE auth.totp_credentials ALTER COLUMN secret_encrypted SET NOT NULL;

ALTER TABLE auth.totp_credentials DROP COLUMN pending_secret_encrypted;

DROP POLICY IF EXISTS auth_configuration_select_any_authenticated ON auth.auth_configuration;

UPDATE auth.auth_configuration SET step_up_actions = '[]'::jsonb WHERE id = 1;

ALTER TABLE auth.auth_configuration ADD COLUMN mandatory_passkey_enrollment BOOLEAN NOT NULL DEFAULT true;
