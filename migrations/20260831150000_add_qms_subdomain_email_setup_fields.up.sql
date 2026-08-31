-- Closes a real gap found during the Phase 2->3 completeness review
-- (2026-08-31): Boris's original ask for Intake/Progress explicitly
-- included "subdomain/email setup", but no column ever captured it --
-- the data sat in raw_ps_snapshot only. Real Highway 20 data confirmed
-- five distinct fields: a company-level subdomain (captured on the
-- "first time" facility's run, same pattern as the other corporate
-- fields), a facility-level subdomain, whether that subdomain already
-- exists in QMS, and a system email distinct from the facility's
-- general contact email (clients.facilities.email).
--
-- subdomain_exists_in_qms_raw stays raw text, not boolean, matching
-- every other Facility-Policies-adjacent yes/no field in this schema
-- (sales_tax_applies_raw, rent_tax_applies_raw, etc.) -- Boris's Phase 1
-- convention of not parsing PS's own answer format into a stricter
-- type than the source data actually is.

ALTER TABLE clients.companies ADD COLUMN subdomain TEXT;

ALTER TABLE clients.facilities ADD COLUMN subdomain TEXT;
ALTER TABLE clients.facilities ADD COLUMN subdomain_exists_in_qms_raw TEXT;
ALTER TABLE clients.facilities ADD COLUMN system_email TEXT;
