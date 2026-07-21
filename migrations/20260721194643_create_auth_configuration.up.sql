CREATE TABLE auth_configuration (
    id SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    mandatory_passkey_enrollment BOOLEAN NOT NULL DEFAULT true,
    allowed_factors JSONB NOT NULL DEFAULT '["webauthn"]',
    step_up_actions JSONB NOT NULL DEFAULT '[]',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE TRIGGER auth_configuration_set_updated_at
    BEFORE UPDATE ON auth_configuration
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();

INSERT INTO auth_configuration (id) VALUES (1);
