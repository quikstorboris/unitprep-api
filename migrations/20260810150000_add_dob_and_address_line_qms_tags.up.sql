-- e.address already covers a single-line address (seeded in
-- 20260808120000). These are the two-line variant plus date of birth,
-- both confirmed directly against the live QMS tag picker by Boris.
INSERT INTO client_ops.qms_tag (tag_key, label, category) VALUES
    ('e.dob', 'Date of Birth', 'Tenant'),
    ('e.add1', 'Address Line 1', 'Tenant'),
    ('e.add2', 'Address Line 2', 'Tenant');
