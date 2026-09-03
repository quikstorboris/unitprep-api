DROP INDEX IF EXISTS clients.companies_active_idx;
ALTER TABLE clients.companies DROP COLUMN IF EXISTS archived_at;
