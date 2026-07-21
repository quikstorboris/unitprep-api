CREATE EXTENSION IF NOT EXISTS citext;

CREATE TYPE auth_role AS ENUM ('admin');
CREATE TYPE user_company AS ENUM ('trojan', 'cobre', 'quikstor');
CREATE TYPE user_status AS ENUM ('invited', 'active', 'deactivated');
CREATE TYPE user_deletion_reason AS ENUM ('offboarding', 'emergency');

CREATE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    email CITEXT NOT NULL UNIQUE,
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULL,
    job_title TEXT,
    company user_company NOT NULL,
    role auth_role NOT NULL DEFAULT 'admin',
    status user_status NOT NULL DEFAULT 'invited',
    deleted_at TIMESTAMPTZ,
    deletion_reason user_deletion_reason,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT deletion_reason_matches_deleted_at
        CHECK ((deleted_at IS NULL) = (deletion_reason IS NULL))
);

CREATE TRIGGER users_set_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();
