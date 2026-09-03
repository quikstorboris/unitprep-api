ALTER TABLE clients.facilities
    DROP COLUMN manually_edited_fields;

ALTER TABLE clients.companies
    DROP COLUMN manually_edited_fields;

ALTER TABLE client_ops.process_street_settings
    ADD COLUMN sync_time TIME NOT NULL DEFAULT '00:00';

ALTER TABLE client_ops.process_street_settings
    DROP COLUMN sync_interval_hours;

DELETE FROM auth.role_permissions WHERE permission_key = 'activity_logs.read';
DELETE FROM auth.permissions WHERE key = 'activity_logs.read';
