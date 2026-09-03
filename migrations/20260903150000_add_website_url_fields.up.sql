-- PS's own `What_is_the_URL_for_this_facility?` (Intake) -- the real
-- business website, distinct from `subdomain` (the QMS-hosted tenant
-- portal URL) and previously not mapped anywhere. Surfaced by a real
-- single-facility business (run rZFNRpmLIxuOrb_8K9hICw) whose Corporate
-- Info section came back entirely blank because the client answered
-- "Yes" to "Is your Corporate Name, Address, Phone Number & Email the
-- same as this Facility?" -- PS skips the dedicated Corporate questions
-- entirely in that case, so there is nothing for `intake_mapping` to
-- read for the Company section at all.
--
-- `clients.facilities.website_url` is the real, PS-sourced value.
-- `clients.companies.website_url` has no PS source field of its own --
-- it exists purely so the confirmation screen's "use this facility's
-- own info as the company's" fallback (offered when Corporate Info
-- came back blank) has somewhere to copy the facility's website into,
-- mirroring the existing corporate_phone/corporate_address_* pattern
-- of a company-level field with no independent Corporate-section
-- source of its own for a single-facility business.
ALTER TABLE clients.facilities
    ADD COLUMN website_url TEXT;

ALTER TABLE clients.companies
    ADD COLUMN website_url TEXT;
