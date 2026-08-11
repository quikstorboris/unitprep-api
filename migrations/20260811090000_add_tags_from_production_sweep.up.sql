-- Harvested from a full sweep of every {{tag}} occurrence across 258
-- already-tagged real production lease documents (the QMS Onboarding
-- Dropbox tree) -- not guessed, not inferred from family-name analogy.
-- Every key here is a literal string this client base has already
-- shipped inside a real client-facing document. Excluded from this
-- batch: 8 keys that were clearly typos/malformed duplicates of tags
-- already in the catalog (e.g. "add1" with no prefix, "e.a..name" with
-- a doubled dot), and 19 keys already seeded in earlier migrations.
--
-- Some labels below are a best-effort guess from the surrounding
-- printed label text in the source document, not a confirmed QMS
-- tooltip -- e.int, m.ins, m.liens, m.vi.vt, m.vi.nor in particular are
-- low-confidence; flagged separately outside this migration for
-- Boris's own tooltip check, not blocking the import.
--
-- New categories introduced here beyond the original Tenant/Unit/
-- Lease/Move-In/Date-Time set: Alternate Contact (e.a.*), Military
-- (e.m.*), Facility (f.*), Company (c.* -- the storage operator's own
-- company, not a tenant's), Vehicle (m.vi.*/l.vi.*), Lienholder
-- (m.opi.*), Signature (sig).
INSERT INTO client_ops.qms_tag (tag_key, label, category) VALUES
    -- Tenant
    ('e.name', 'Full Name', 'Tenant'),
    ('e.mil', 'Is Military Profile Present', 'Tenant'),
    ('e.init', 'Initials', 'Tenant'),
    ('e.comp', 'Company Name (if Tenant is a business)', 'Tenant'),
    ('e.zip', 'Zip Code', 'Tenant'),
    ('e.keycodes', 'Access/Gate Codes', 'Tenant'),
    ('e.dlexp', 'Driver License Expiration', 'Tenant'),
    ('e.alt', 'Is Alternate Contact Present', 'Tenant'),
    ('e.title', 'Title', 'Tenant'),
    ('e.int', 'Interest', 'Tenant'),

    -- Alternate Contact
    ('e.a.name', 'Name', 'Alternate Contact'),
    ('e.a.add1', 'Address Line 1', 'Alternate Contact'),
    ('e.a.add2', 'Address Line 2', 'Alternate Contact'),
    ('e.a.phone', 'Phone Number', 'Alternate Contact'),
    ('e.a.state', 'State', 'Alternate Contact'),
    ('e.a.city', 'City', 'Alternate Contact'),
    ('e.a.post', 'Postal Code', 'Alternate Contact'),
    ('e.a.email', 'Email', 'Alternate Contact'),
    ('e.a.address', 'Address', 'Alternate Contact'),
    ('e.a.zip', 'Zip Code', 'Alternate Contact'),
    ('e.a.rel', 'Relationship to Tenant', 'Alternate Contact'),
    ('e.a.fname', 'First Name', 'Alternate Contact'),
    ('e.a.lname', 'Last Name', 'Alternate Contact'),

    -- Military
    ('e.m.cophone', 'Commanding Officer Phone', 'Military'),
    ('e.m.branch', 'Branch of Service', 'Military'),
    ('e.m.colname', 'Commanding Officer Last Name', 'Military'),
    ('e.m.cofname', 'Commanding Officer First Name', 'Military'),
    ('e.m.id', 'Military ID Number', 'Military'),
    ('e.m.eserv', 'End of Service Date', 'Military'),
    ('e.m.unit', 'Military Unit', 'Military'),
    ('e.m.sserv', 'Start of Service Date', 'Military'),
    ('e.m.a.lname', 'Agent Last Name', 'Military'),
    ('e.m.a.fname', 'Agent First Name', 'Military'),

    -- Facility
    ('f.name', 'Facility Name', 'Facility'),
    ('f.add1', 'Address Line 1', 'Facility'),
    ('f.add2', 'Address Line 2', 'Facility'),
    ('f.state', 'State', 'Facility'),
    ('f.city', 'City', 'Facility'),
    ('f.post', 'Postal Code', 'Facility'),
    ('f.phone', 'Phone Number', 'Facility'),
    ('f.email', 'Email', 'Facility'),
    ('f.address', 'Address', 'Facility'),
    ('f.porturl', 'Tenant Portal URL', 'Facility'),
    ('f.ow.firstname', 'Owner First Name', 'Facility'),
    ('f.ow.lastname', 'Owner Last Name', 'Facility'),

    -- Company (the self-storage operator's own company, not a tenant's)
    ('c.name', 'Company Name', 'Company'),
    ('c.email', 'Company Email', 'Company'),

    -- Unit
    ('u.dim', 'Dimensions', 'Unit'),
    ('u.length', 'Length', 'Unit'),
    ('u.width', 'Width', 'Unit'),
    ('u.type', 'Type', 'Unit'),
    ('u.stdrate', 'Standard Rate', 'Unit'),
    ('u.area', 'Area', 'Unit'),

    -- Move-In
    ('m.ptd', 'Paid Through Date', 'Move-In'),
    ('m.ptd+1', 'Paid Through Date + 1 Day', 'Move-In'),
    ('m.descgood', 'Description of Goods', 'Move-In'),
    ('m.insprice', 'Insurance Price', 'Move-In'),
    ('m.promo.name', 'Promotion Name', 'Move-In'),
    ('m.maxra', 'Max Rent Increase Amount', 'Move-In'),
    ('m.ins', 'Insurance', 'Move-In'),
    ('m.leadsrc', 'Lead Source', 'Move-In'),
    ('m.liens', 'Liens', 'Move-In'),

    -- Lease
    ('l.nxtamt', 'Next Payment Amount', 'Lease'),
    ('l.ptd+1', 'Paid Through Date + 1 Day', 'Lease'),
    ('l.baldue', 'Balance Due', 'Lease'),
    ('l.effrate', 'Monthly Rent', 'Lease'),

    -- Vehicle (move-in and lease context both real; the m.*/l.*
    -- duality applies to vehicle info too, not just lease dates)
    ('m.vi.pn', 'License Plate Number', 'Vehicle'),
    ('m.vi.model', 'Model', 'Vehicle'),
    ('m.vi.make', 'Make', 'Vehicle'),
    ('m.vi.ps', 'License Plate State', 'Vehicle'),
    ('m.vi.year', 'Year', 'Vehicle'),
    ('m.vi.vin', 'VIN', 'Vehicle'),
    ('m.vi.lhfn', 'Lienholder Name', 'Vehicle'),
    ('m.vi.lha', 'Lienholder Address', 'Vehicle'),
    ('m.vi.vt', 'Vehicle Type', 'Vehicle'),
    ('m.vi.ipn', 'Insurance Policy Number', 'Vehicle'),
    ('m.vi.note', 'Note', 'Vehicle'),
    ('m.vi.nor', 'Notice of Removal', 'Vehicle'),
    ('m.vi.color', 'Color', 'Vehicle'),
    ('m.vi.ii', 'Insurance Issuer', 'Vehicle'),
    ('l.vi.ps', 'License Plate State', 'Vehicle'),
    ('l.vi.pn', 'License Plate Number', 'Vehicle'),
    ('l.vi.year', 'Year', 'Vehicle'),
    ('l.vi.color', 'Color', 'Vehicle'),
    ('l.vi.ied', 'Insurance Expiration Date', 'Vehicle'),
    ('l.vi.ii', 'Insurance Issuer', 'Vehicle'),
    ('l.vi.vin', 'VIN', 'Vehicle'),
    ('l.vi.ipn', 'Insurance Policy Number', 'Vehicle'),
    ('l.vi.lha', 'Lienholder Address', 'Vehicle'),
    ('l.vi.model', 'Model', 'Vehicle'),
    ('l.vi.make', 'Make', 'Vehicle'),
    ('l.vi.lhpn', 'Lienholder Phone Number', 'Vehicle'),
    ('l.vi.lhfn', 'Lienholder Name', 'Vehicle'),

    -- Lienholder (the more common real-world convention -- see far
    -- higher usage counts than the m.vi.lh*/l.vi.lh* variants above)
    ('m.opi.lhfn', 'Lienholder Name', 'Lienholder'),
    ('m.opi.lha', 'Lienholder Address', 'Lienholder'),
    ('m.opi.desc', 'Property Description', 'Lienholder'),
    ('m.opi.lhpn', 'Lienholder Phone Number', 'Lienholder'),

    -- Signature
    ('sig', 'Electronic Signature', 'Signature');
