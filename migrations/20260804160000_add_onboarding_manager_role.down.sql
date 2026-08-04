-- Postgres has no DROP VALUE for an enum -- the only way back is to
-- recreate the type without it, which only makes sense if nothing
-- actually used it. Refuses loudly rather than silently reassigning or
-- deleting a real user's role.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM auth.users WHERE role = 'onboarding_manager') THEN
        RAISE EXCEPTION 'cannot revert: at least one user has role = onboarding_manager';
    END IF;
END
$$;

ALTER TYPE auth.auth_role RENAME TO auth_role_old;

CREATE TYPE auth.auth_role AS ENUM ('admin');

ALTER TABLE auth.users
    ALTER COLUMN role DROP DEFAULT,
    ALTER COLUMN role TYPE auth.auth_role USING role::text::auth.auth_role,
    ALTER COLUMN role SET DEFAULT 'admin';

DROP TYPE auth.auth_role_old;
