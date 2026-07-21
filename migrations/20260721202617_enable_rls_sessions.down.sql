DROP FUNCTION IF EXISTS resolve_session(BYTEA);
DROP FUNCTION IF EXISTS create_session(UUID, BYTEA, TIMESTAMPTZ, INET, TEXT);
DROP POLICY IF EXISTS sessions_insert_blocked ON sessions;
DROP POLICY IF EXISTS sessions_update_own_or_admin ON sessions;
DROP POLICY IF EXISTS sessions_select_own_or_admin ON sessions;
ALTER TABLE sessions DISABLE ROW LEVEL SECURITY;
