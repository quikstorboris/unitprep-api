-- Second seed batch, from the same Sumas Mini Storage template the
-- first batch (20260812190000) drew from. These four were left out of
-- that first pass not because they're ambiguous, but out of excess
-- caution -- re-checked against the real flattened text and each
-- appears exactly once in this document:
--   - "DATE" and "CONTACT PHONE" are genuinely unique labels.
--   - "ST" (state) was skipped for being short/generic; kept anyway
--     since a real occurrence justifies it per the growth philosophy,
--     same as every other tag in this catalog.
--   - "ALTERNATE NAME" is the alternate contact's own distinct label
--     (unlike "ADDRESS"/"PHONE"/"EMAIL ADDRESS", which this document
--     genuinely reuses verbatim for both the occupant and the
--     alternate contact -- still deliberately not seeded; see this
--     migration's own follow-up note in the vault).
--
-- Worth remembering as a class of risk, not just for these four: a
-- short, generic label can match as a *substring* of a more specific
-- one elsewhere, since label matching only checks word boundaries, not
-- whether the match sits inside a longer phrase. "PHONE" bare would
-- also match inside "CONTACT PHONE" itself (a space and an underscore
-- are both valid word boundaries) -- confirmed by hand before ruling it
-- out, which is exactly why it's still not seeded here even though
-- "CONTACT PHONE" itself is.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('d.now', 'label_proximity',
        '{"label": "DATE", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage'),
    ('e.phone', 'label_proximity',
        '{"label": "CONTACT PHONE", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage'),
    ('e.state', 'label_proximity',
        '{"label": "ST", "position": "after", "max_gap_chars": 3}',
        'Sumas Mini Storage -- short/generic label, watch for false positives as more documents are seen'),
    ('e.a.name', 'label_proximity',
        '{"label": "ALTERNATE NAME", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage');
