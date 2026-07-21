DROP FUNCTION IF EXISTS consume_invite(BYTEA);
DROP FUNCTION IF EXISTS resolve_invite(BYTEA);
ALTER TABLE user_invites ALTER COLUMN created_by DROP DEFAULT;
DROP POLICY IF EXISTS user_invites_admin_only ON user_invites;
ALTER TABLE user_invites DISABLE ROW LEVEL SECURITY;
