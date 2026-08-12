-- Third seed batch. Solves the exact gap the first batch's own notes
-- flagged as deferred: "ADDRESS:"/"PHONE"/"EMAIL ADDRESS" in Sumas's
-- template genuinely are reused verbatim for both the occupant and the
-- alternate contact -- previously left unseeded because a bare label
-- pattern has no way to tell the two occurrences apart.
--
-- requires_preceding_anchor (added to the label_proximity pattern shape
-- in this same session -- see unitprep-tagger-tagger-pipeline's
-- SubstitutionStyle-adjacent recognize.rs changes) fixes this: a
-- pattern only matches a label occurrence if a second, distinct anchor
-- label appears somewhere in the within_chars immediately *before* it.
-- Every window below was measured against the real document's actual
-- flattened text (not guessed) and includes a healthy margin:
--   OCCUPANT NAME (byte 146) -> ADDRESS: (231): 85 chars, window 120
--   ALTERNATE NAME (753) -> ADDRESS: (836): 83 chars, window 120
--   CONTACT PHONE (502) -> EMAIL ADDRESS (536): 34 chars, window 60
--   ALTERNATE NAME (753) -> EMAIL ADDRESS (948): 195 chars, window 220
--   ALTERNATE NAME (753) -> PHONE (921): 168 chars, window 200
--
-- e.phone (primary) was already seeded unanchored in the previous
-- migration -- "CONTACT PHONE" is a genuinely unique label in this
-- document, unlike alternate's bare "PHONE" (which would otherwise
-- ALSO substring-match inside "CONTACT PHONE" itself -- confirmed by
-- hand; a space and an underscore both satisfy the word-boundary
-- check either side of "PHONE"). The anchor on e.a.phone's own pattern
-- happens to close that exact gap too: "ALTERNATE NAME" (753) never
-- precedes the "PHONE" occurrence inside "CONTACT PHONE" (position
-- 510, entirely before 753), so it's correctly excluded without
-- needing any special-casing beyond the anchor mechanism itself.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('e.address', 'label_proximity',
        '{"label": "ADDRESS:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "OCCUPANT NAME", "within_chars": 120}}',
        'Sumas Mini Storage -- occupant''s own address, disambiguated from the alternate contact''s identical "ADDRESS:" label'),
    ('e.a.address', 'label_proximity',
        '{"label": "ADDRESS:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "ALTERNATE NAME", "within_chars": 120}}',
        'Sumas Mini Storage'),
    ('e.email', 'label_proximity',
        '{"label": "EMAIL ADDRESS", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "CONTACT PHONE", "within_chars": 60}}',
        'Sumas Mini Storage'),
    ('e.a.email', 'label_proximity',
        '{"label": "EMAIL ADDRESS", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "ALTERNATE NAME", "within_chars": 220}}',
        'Sumas Mini Storage'),
    ('e.a.phone', 'label_proximity',
        '{"label": "PHONE", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "ALTERNATE NAME", "within_chars": 200}}',
        'Sumas Mini Storage -- bare "PHONE" would otherwise also substring-match inside "CONTACT PHONE"; the anchor rules that out too');
