-- "Implementation Completed" for `clients.companies` -- set by a person on
-- the company page once onboarding is finished. The Clients page moves a
-- completed company from "Implementations in Flight" to a collapsed
-- "Completed Implementations" section. A soft flag like `archived_at`
-- (20260901120000): nothing about the company's data changes, and
-- reopening is just clearing the timestamp. No RLS change -- a plain
-- UPDATE on a table whose UPDATE policy already gates to the client-ops
-- roles. No index: the list page reads every company and splits them in
-- the app, as it already does for archived ones.
ALTER TABLE clients.companies
    ADD COLUMN implementation_completed_at TIMESTAMPTZ NULL;
