-- SiteLink's tenant export, for dedup (content_type = 'tenants'). Source
-- reports are SiteLink's "Directory" and "Rent Roll" (both xlsx), first
-- seen on the LG Squared LLC preliminary pull, 2026-10-01. Of SiteLink's
-- ~40 reports these are the only two that carry tenant name + address +
-- phone + email + unit + ledger on one row; every other report lacks the
-- contact columns dedup compares.
--
-- Shape: one row per active ledger (a tenant's lease on one unit), so a
-- tenant renting several units repeats -- the same per-unit shape QSX
-- has. LedgerID is unique per row and maps to CustNumb; sUnitName maps
-- to UnitNumber. TenantID (SiteLink's real tenant identity) is NOT
-- mapped: dedup has no field for it, and two rows sharing a TenantID
-- share one contact record, so they can never disagree.
--
-- There is no single name or phone column, so `FirtLast` (the grouping
-- key) and `PhoneNumber` are derived by the named transform
-- `derive_sitelink_tenant_fields` (core::vendor_format::transforms),
-- which also drops Rent Roll's vacant-unit rows (no LedgerID). The
-- identity pairs below pick up those derived columns, same arrangement
-- as the other registered tenant vendors.
--
-- Alternate contact comes straight from the tenant record's *Alt fields.
-- Left unmapped (dropped, not blank): the Business (*Bus) and Additional
-- (*Add) contact blocks, which are almost empty in the sample and have no
-- dedup counterpart; sMobile except as the sPhone fallback.
--
-- Signature is headers present in BOTH reports (Directory also has
-- TenantName, Rent Roll does not), so either one is recognized.
INSERT INTO client_ops.vendor_format (name, content_type, signature_headers, field_mapping, transform_key) VALUES
('SiteLink', 'tenants', ARRAY['sUnitName','LedgerID','TenantID','sFName','sLName','sAddr1','sEmail'], '[
    {"target":"CustNumb","source":"LedgerID"},
    {"target":"UnitNumber","source":"sUnitName"},
    {"target":"FirtLast","source":"FirtLast"},
    {"target":"FirstName","source":"sFName"},
    {"target":"LastName","source":"sLName"},
    {"target":"CompanyName","source":"sCompany"},
    {"target":"PhoneNumber","source":"PhoneNumber"},
    {"target":"Email","source":"sEmail"},
    {"target":"AddressStreet1","source":"sAddr1"},
    {"target":"AddressStreet2","source":"sAddr2"},
    {"target":"AddressCity","source":"sCity"},
    {"target":"AddressState","source":"sRegion"},
    {"target":"AddressPostalCode","source":"sPostalCode"},
    {"target":"AlternateContactFirstName","source":"sFNameAlt"},
    {"target":"AlternateContactLastName","source":"sLNameAlt"},
    {"target":"AlternateContactEmail","source":"sEmailAlt"},
    {"target":"AlternateContactPhoneNumber","source":"sPhoneAlt"},
    {"target":"AlternateContactAddressStreet1","source":"sAddr1Alt"},
    {"target":"AlternateContactAddressStreet2","source":"sAddr2Alt"},
    {"target":"AlternateContactAddressCity","source":"sCityAlt"},
    {"target":"AlternateContactAddressState","source":"sRegionAlt"},
    {"target":"AlternateContactAddressPostalCode","source":"sPostalCodeAlt"}
]'::jsonb, 'derive_sitelink_tenant_fields');
