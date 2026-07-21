DROP POLICY IF EXISTS users_delete_blocked ON users;
DROP POLICY IF EXISTS users_insert_admin_only ON users;
DROP POLICY IF EXISTS users_update_own_or_admin ON users;
DROP POLICY IF EXISTS users_select_own_or_admin ON users;
ALTER TABLE users DISABLE ROW LEVEL SECURITY;
