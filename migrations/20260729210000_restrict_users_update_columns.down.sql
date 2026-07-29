-- Restores the original (over-broad) table-level UPDATE grant. The
-- column-level grant must be revoked first: table-level and column-level
-- privileges coexist rather than override, so leaving the narrow grant in
-- place would make the resulting ACL a superset of the pre-migration
-- state rather than a match for it.
REVOKE UPDATE (first_name, last_name, job_title) ON auth.users FROM app_service;
GRANT UPDATE ON auth.users TO app_service;
