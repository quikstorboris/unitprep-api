ALTER TABLE client_ops.process_street_settings
    DROP CONSTRAINT IF EXISTS process_street_settings_daily_time_requires_time_and_zone;

ALTER TABLE client_ops.process_street_settings DROP COLUMN IF EXISTS sync_timezone;
ALTER TABLE client_ops.process_street_settings DROP COLUMN IF EXISTS sync_time;
ALTER TABLE client_ops.process_street_settings DROP COLUMN IF EXISTS schedule_mode;
