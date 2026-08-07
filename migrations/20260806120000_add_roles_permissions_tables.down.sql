-- Dropping the tables cascades away their own policies automatically;
-- the helper function has to go after, since the policies above depend
-- on it.
DROP TABLE auth.user_roles;
DROP TABLE auth.role_permissions;
DROP TABLE auth.permissions;
DROP TABLE auth.roles;
DROP FUNCTION auth.current_user_has_role(TEXT);
