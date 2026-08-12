-- "SIZE" was entirely unrecognized in the real Sumas document --
-- confirmed by inspecting a live-tested export: "SIZE_______________________CITY"
-- was left untouched, no pattern to match it. "SIZE" here means the
-- unit's dimensions (e.g. "10x20"), not square footage, so it maps to
-- the existing u.dim tag rather than u.area. The label sits directly
-- against its blank with zero gap ("SIZE___..."), and "SIZE" occurs
-- exactly once in the document -- unambiguous, unanchored.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('u.dim', 'label_proximity',
        '{"label": "SIZE", "position": "after", "max_gap_chars": 5}',
        'Sumas Mini Storage -- unit dimensions, e.g. "10x20"');
