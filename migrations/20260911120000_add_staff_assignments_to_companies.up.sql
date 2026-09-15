-- Implementation Manager & Sales Rep: two new per-company business
-- concepts sourced from Process Street's "Who is the conductor" (renamed
-- Implementation Manager in this app -- that word must never appear in
-- the UI) and "Fill in Rep Only fields" Intake steps. Tracked on
-- clients.companies, not clients.facilities, even though each facility
-- can technically carry its own PS run -- both concepts are company-wide
-- per Boris's explicit direction.
--
-- Same nullable-FK-to-auth.users-with-ON-DELETE-SET-NULL shape as
-- client_ops.vendor_format.created_by: neither assignment is load-bearing
-- enough to block a company row from existing or cascading away, and
-- losing the assigned user (offboarding) should silently clear the
-- assignment rather than orphan or cascade-delete a real client.
--
-- Actually populating these two columns (PS field mapping + a one-time
-- backfill for existing clients) is a deliberate follow-up once the exact
-- PS field shape for the conductor/rep steps is confirmed -- see
-- src/clients/staff_resolution.rs's own module doc. These columns exist
-- now so the schema, resolution machinery, and read endpoints can be
-- built and reviewed independently of that mapping work.
ALTER TABLE clients.companies
    ADD COLUMN implementation_manager_user_id UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    ADD COLUMN sales_rep_user_id UUID REFERENCES auth.users(id) ON DELETE SET NULL;
