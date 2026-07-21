DROP TRIGGER IF EXISTS users_set_updated_at ON users;
DROP TABLE IF EXISTS users;
DROP FUNCTION IF EXISTS set_updated_at();
DROP TYPE IF EXISTS user_deletion_reason;
DROP TYPE IF EXISTS user_status;
DROP TYPE IF EXISTS user_company;
DROP TYPE IF EXISTS auth_role;
