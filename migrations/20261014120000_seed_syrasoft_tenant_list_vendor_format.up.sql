-- Syrasoft's Tenant List for dedup (content_type = 'tenants'). First seen
-- on the A2Z Mini Storage / Longview Self Storage / TC's RV & Mini Storage
-- preliminary pulls (2026-10-14).
--
-- Syrasoft's reports are printed layouts, not data exports. "Tenant List -
-- All Tenants" stacks a block of lines per tenant record (name, up to
-- several typed phones, address, email, comments, and a table of that
-- record's units). `unitprep_core::parsing::syrasoft_report` flattens it
-- to one row per (tenant record, unit) -- reading every sheet, since a
-- large facility's report runs onto a second one -- and leaves out the
-- former tenants ("All Tenants" lists every record ever created: 6,000
-- for 530 current tenants at Longview). The headers below are that
-- flattened shape.
--
-- Of Syrasoft's reports this is the only one with name + phones + address
-- + email + every unit on one record. All Units By Unit # repeats the
-- same contact per unit but leaves out rented units of other types
-- (mailboxes, company units) and carries one phone; the Rent Roll, the
-- email and cell-phone reports and the collection reports have no
-- address. None of them are dedup files.
--
-- No tenant id exists in any Syrasoft report, so `Tenant Record ID`
-- (supplied by the flattener) is "R-" plus the record's first unit. Tenants are
-- therefore grouped by record, and one person entered as two records
-- (same name, two records) is reported as a duplicate customer record --
-- the case Syrasoft pulls actually have (Betty Kreger at A2Z, 12 at
-- Longview, 11 at TC's). CustNumb / UnitNumber are the unit, as for
-- Winsen.
--
-- Not mapped: alternate contacts. They are not in this pull; Syrasoft
-- needs a separate export for them (still to be re-pulled), and this row
-- gets its alternate-contact entries when that file is seen.
--
-- Derived by the named transform `derive_syrasoft_tenant_fields`:
-- FirtLast ("Last, First" -> "First Last"), PhoneNumber (home, then cell,
-- then business; staff notes after the number cut off), and the address
-- split into street / city / state / zip. Email is blanked where it holds
-- a staff note instead of an address.
INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
VALUES
('Syrasoft Tenant List', 'tenants',
 ARRAY['Tenant','Tenant Record ID','Unit #','Unit Type','Phone 1','Address Line 1'], '[
    {"target":"CustNumb","source":"Unit #"},
    {"target":"UnitNumber","source":"Unit #"},
    {"target":"TenantId","source":"Tenant Record ID"},
    {"target":"FirtLast","source":"FirtLast"},
    {"target":"PhoneNumber","source":"PhoneNumber"},
    {"target":"Email","source":"Email"},
    {"target":"AddressStreet1","source":"AddressStreet1"},
    {"target":"AddressStreet2","source":"AddressStreet2"},
    {"target":"AddressCity","source":"AddressCity"},
    {"target":"AddressState","source":"AddressState"},
    {"target":"AddressPostalCode","source":"AddressPostalCode"}
]'::jsonb, 'derive_syrasoft_tenant_fields',
 'Syrasoft', 'Tenant List - All Tenants', 'primary', 0,
 'The Syrasoft "Tenant List - All Tenants" report: one row per unit of each current tenant, with the name, phone, email and address from the tenant record. This one file is all that is needed (a large facility''s report runs onto a second sheet; both are read). Syrasoft has no tenant ids, so a record is identified by R- and its first unit (for example R-0043), and the same person entered as two records is reported as a duplicate customer record. Alternate contacts are not in this report and are not checked yet. The other Syrasoft reports (All Units By Unit #, Rent Roll, email and cell-phone lists) are not dedup files.');
