ALTER TABLE users ENABLE ROW LEVEL SECURITY;

CREATE POLICY users_select_own_or_admin ON users
    FOR SELECT
    USING (
        id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

CREATE POLICY users_update_own_or_admin ON users
    FOR UPDATE
    USING (
        id = NULLIF(current_setting('app.current_user_id', true), '')::uuid
        OR current_setting('app.current_user_role', true) = 'admin'
    );

CREATE POLICY users_insert_admin_only ON users
    FOR INSERT
    WITH CHECK (current_setting('app.current_user_role', true) = 'admin');

CREATE POLICY users_delete_blocked ON users
    FOR DELETE
    USING (false);
