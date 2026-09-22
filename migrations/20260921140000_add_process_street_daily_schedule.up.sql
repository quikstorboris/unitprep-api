-- Brings back a fixed-clock-time schedule option alongside the plain
-- hourly interval added 2026-09-02 -- Boris's call, 2026-09-21: some
-- syncs should run at a specific time of day in a specific timezone
-- (e.g. overnight, or aligned with a specific office's hours) rather
-- than "every N hours from whenever the server last ticked." Unlike
-- the original `sync_time` column dropped in
-- 20260902120000_activity_logs_and_configurable_sync, this one also
-- carries a real IANA timezone (`sync_timezone`) so the configured
-- clock time actually shifts correctly across Daylight Saving Time,
-- rather than being a fixed UTC instant that silently drifts an hour
-- twice a year relative to local business hours.
--
-- Both scheduling modes stay available (`schedule_mode`), not a
-- replacement -- an admin picks one. `sync_interval_hours` keeps its
-- existing default/range regardless of which mode is active; it's
-- simply ignored while `schedule_mode = 'daily_time'`, same as
-- `sync_time`/`sync_timezone` are ignored while `schedule_mode =
-- 'interval'`. See `clients::sync`'s own module doc for how the
-- background task reads this.
ALTER TABLE client_ops.process_street_settings
    ADD COLUMN schedule_mode TEXT NOT NULL DEFAULT 'interval'
        CHECK (schedule_mode IN ('interval', 'daily_time'));

ALTER TABLE client_ops.process_street_settings
    ADD COLUMN sync_time TIME;

-- A closed set of IANA zone names, not a free-text field -- matches
-- the fixed dropdown Boris asked for (PST/MST/CST/EST/UTC plus
-- Serbia's own zone), validated against the same list in
-- api::process_street_settings so the UI and the backend can never
-- disagree about what's a valid choice. America/Los_Angeles etc.
-- (not a fixed UTC offset) so DST is handled correctly automatically.
ALTER TABLE client_ops.process_street_settings
    ADD COLUMN sync_timezone TEXT
        CHECK (sync_timezone IS NULL OR sync_timezone IN (
            'America/Los_Angeles', 'America/Denver', 'America/Chicago',
            'America/New_York', 'UTC', 'Europe/Belgrade'
        ));

ALTER TABLE client_ops.process_street_settings
    ADD CONSTRAINT process_street_settings_daily_time_requires_time_and_zone
    CHECK (schedule_mode <> 'daily_time' OR (sync_time IS NOT NULL AND sync_timezone IS NOT NULL));
