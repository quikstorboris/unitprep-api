-- Durable, per-facility counter backing the dedup export filename
-- convention `{ABBREV}_v{N}_pull_check_{MM-DD-YYYY}.{ext}` (see
-- src/clients/dedup_filename.rs). Starts at 0 and is incremented
-- atomically (`UPDATE ... RETURNING`) by that module at export time, so
-- the first export ever produced for a facility is v1, not v0 -- and the
-- count never resets, regardless of date.
ALTER TABLE clients.facilities
    ADD COLUMN dedup_export_sequence INTEGER NOT NULL DEFAULT 0;
