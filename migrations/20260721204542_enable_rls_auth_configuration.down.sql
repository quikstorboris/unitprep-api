DROP POLICY IF EXISTS auth_configuration_admin_only ON auth_configuration;
ALTER TABLE auth_configuration DISABLE ROW LEVEL SECURITY;
