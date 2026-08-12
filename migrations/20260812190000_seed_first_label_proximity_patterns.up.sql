-- First real rows in client_ops.tag_pattern (created empty in
-- 20260811210000). Same growth philosophy already established for
-- qms_tag itself: one row per confirmed real phrasing, seeded here from
-- three genuinely raw templates found in the corpus (Rest Stop Storage,
-- Tri County Mini Storage, Sumas Mini Storage -- see the vault's QMS
-- Template Tagging Assistant notes for provenance).
--
-- Deliberately conservative: every label below is unambiguous *within
-- the document it was found in* and confirmed to map cleanly to one
-- existing tag. Several real labels seen in the same documents are
-- skipped on purpose, not overlooked -- "ADDRESS", "PHONE", and "EMAIL
-- ADDRESS" each appear twice in Sumas's template alone (once for the
-- occupant, once for an alternate contact), and a label_proximity
-- pattern has no notion of *which* occurrence it's near. Seeding one of
-- those now would auto-apply a confident-looking but genuinely
-- coin-flip guess at whichever blank it happens to match first -- worse
-- than not recognizing it at all. Revisit once patterns can reason
-- about surrounding section context, not before.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('e.init', 'label_proximity',
        '{"label": "Customer''s Initials", "position": "after", "max_gap_chars": 5}',
        'Rest Stop Storage'),
    ('e.name', 'label_proximity',
        '{"label": "OCCUPANT NAME", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage'),
    ('e.name', 'label_proximity',
        '{"label": "SECOND PARTY", "position": "after", "max_gap_chars": 5}',
        'Tri County Mini Storage'),
    ('u.num', 'label_proximity',
        '{"label": "UNIT #", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage'),
    ('e.city', 'label_proximity',
        '{"label": "CITY", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage'),
    ('e.post', 'label_proximity',
        '{"label": "ZIP CODE", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage'),
    ('m.secdep', 'label_proximity',
        '{"label": "SECURITY DEPOSIT", "position": "after", "max_gap_chars": 10}',
        'Sumas Mini Storage -- gap covers the literal "$" between label and blank');
