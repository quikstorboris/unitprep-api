-- Tracks where each facility_people link came from -- 'process_street'
-- (the existing "Add User" chip flow, and every row already ingested)
-- or 'manual' (a brand-new person typed in by hand, or a
-- Process-Street-sourced person a manager chose to protect while
-- editing them -- see api::clients_facility_people's own module doc,
-- 2026-09-08). A 'manual' row is permanently exempt from the Users
-- tab's own self-heal pass (get_facility_people silently refreshing a
-- roster row from clients.ps_person_index) the same way a QSX-exempt
-- policy category is exempt from a future policy-sync -- 'process_street'
-- rows keep self-healing exactly as they do today.
ALTER TABLE clients.facility_people
    ADD COLUMN source TEXT NOT NULL DEFAULT 'process_street' CHECK (source IN ('process_street', 'manual'));
