-- Winsen (Sentinel) tenant reports for dedup. Winsen exports printed
-- reports, not one tenant table: the contact details, the email address
-- and the customer id each live in a different report. Boris's Highway 20
-- pull (2026-10-02) is the sample. A run reads the contact report as the
-- main file and joins the other two onto it by unit + customer name
-- (see unitprep_dedup::join). The reports are flattened from their page
-- layout before they get here (unitprep_core::parsing::printed_report),
-- so these signatures are the flattened header labels.
--
-- New file role 'join': more fields for the tenants in the same system's
-- primary file. The existing CHECK on file_role has to allow it.
ALTER TABLE client_ops.vendor_format
    DROP CONSTRAINT IF EXISTS vendor_format_file_role_check;
ALTER TABLE client_ops.vendor_format
    ADD CONSTRAINT vendor_format_file_role_check
    CHECK (file_role IN ('primary', 'supporting', 'join'));

-- Customer Name is one column ("First Last"), so FirtLast is a straight
-- rename; the first/last split is left blank and the display name falls
-- back to the title-cased FirtLast. CustNumb is the unit: Winsen's own
-- customer id comes from the rent roll below (TenantId). The business
-- phone has no canonical field and is left out.
INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
VALUES
('Winsen Tenant Cross Reference', 'tenants',
 ARRAY['Unit','Customer Name','Address Line 1','City','State','Zip'], '[
    {"target":"CustNumb","source":"Unit"},
    {"target":"UnitNumber","source":"Unit"},
    {"target":"FirtLast","source":"Customer Name"},
    {"target":"AddressStreet1","source":"Address Line 1"},
    {"target":"AddressStreet2","source":"Address Line 2"},
    {"target":"AddressCity","source":"City"},
    {"target":"AddressState","source":"State"},
    {"target":"AddressPostalCode","source":"Zip"},
    {"target":"PhoneNumber","source":"Res. Phone"}
]'::jsonb, NULL,
 'Winsen', 'Tenant Cross Reference', 'primary', 0,
 'The Winsen "Tenant Cross Reference" report: one row per unit with the tenant''s name, address and phone. This is the main file; select the two reports below with it.'),
('Winsen Tenant Email Address Report', 'tenants',
 ARRAY['Unit','Customer Name','Customer Email Address'], '[
    {"target":"UnitNumber","source":"Unit"},
    {"target":"FirtLast","source":"Customer Name"},
    {"target":"Email","source":"Customer Email Address"}
]'::jsonb, NULL,
 'Winsen', 'Tenant Email Address Report', 'join', 0,
 'The Winsen "Tenant Email Address Report": adds each tenant''s email address. Matched to the Tenant Cross Reference by unit and customer name.'),
('Winsen Rent Roll Report', 'tenants',
 ARRAY['Unit','Customer Name','Cust ID'], '[
    {"target":"UnitNumber","source":"Unit"},
    {"target":"FirtLast","source":"Customer Name"},
    {"target":"TenantId","source":"Cust ID"}
]'::jsonb, NULL,
 'Winsen', 'Rent Roll Report', 'join', 0,
 'The Winsen "Rent Roll Report": adds the customer id (Cust ID) that tells one customer from another. Tenants not on the rent roll have no id and are listed separately, for you to match by name or ignore. Matched to the Tenant Cross Reference by unit and customer name.');
