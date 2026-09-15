REVOKE EXECUTE ON FUNCTION auth.staff_directory() FROM app_service;
DROP FUNCTION auth.staff_directory();

DROP TABLE clients.staff_identity_alias;
