ALTER TABLE clients.facilities DROP COLUMN IF EXISTS system_email;
ALTER TABLE clients.facilities DROP COLUMN IF EXISTS subdomain_exists_in_qms_raw;
ALTER TABLE clients.facilities DROP COLUMN IF EXISTS subdomain;

ALTER TABLE clients.companies DROP COLUMN IF EXISTS subdomain;
