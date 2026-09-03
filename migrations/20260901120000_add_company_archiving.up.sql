-- Archiving for `clients.companies` -- an archived company (and its
-- facilities, implicitly, since they're only ever reached through it)
-- stops appearing on the main `/clients` list, moving to a collapsed
-- "Archived" section instead. A soft flag, not a delete: nothing about
-- the real PS-sourced data changes, and un-archiving is just clearing
-- the timestamp. No RLS change needed -- this is a plain UPDATE on a
-- table whose UPDATE policy already gates to onboarding_manager/
-- department_manager (see 20260828120000's own comment).
ALTER TABLE clients.companies
    ADD COLUMN archived_at TIMESTAMPTZ NULL;

-- Partial index -- only the (typically small) active set is ever
-- listed by default, so this is the index the list page's own query
-- actually wants; archived rows fall through to a full scan on the
-- rare "show archived too" request, which is fine at this table's
-- real scale.
CREATE INDEX companies_active_idx ON clients.companies (legal_name) WHERE archived_at IS NULL;
