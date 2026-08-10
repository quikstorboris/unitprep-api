-- Grows client_ops.qms_tag past its original 13-row seed with tags
-- identified against a real sample lease (Affordable Storage,
-- FM-Lease---Dec-2025.pdf) -- see the vault's tags-effort notes for the
-- document analysis these were drawn from. Each key here is directly
-- named (more than once) in the vault's own transcribed tag-family
-- notes, not guessed -- unlike vehicle/RV/boat sub-tags and business/
-- company tags, which stay unadded until their exact key names are
-- confirmed against QMS's live tooltips.
--
-- m.indate/l.indate and m.secdep/l.secdep are the same two underlying
-- concepts (move-in date, security deposit) under the two different
-- prefixes the same fact carries depending on which point in the
-- lease's life a document refers to it -- see the "m.*/l.* duality" note
-- in the tag catalog design doc. Both members of each pair are added
-- together since either could show up in a given document's wording.
INSERT INTO client_ops.qms_tag (tag_key, label, category) VALUES
    ('m.indate', 'Move-In Date', 'Move-In'),
    ('m.secdep', 'Security Deposit', 'Move-In'),
    ('l.indate', 'Move-In Date', 'Lease'),
    ('l.secdep', 'Security Deposit', 'Lease'),
    ('d.now', 'Today''s Date', 'Date/Time'),
    ('d.nowlong', 'Today''s Date (Long Form)', 'Date/Time');
