-- Label patterns for fields whose only place to write a value is the end of
-- the label's own floating text box (a form converted from PDF: one box per
-- label over a drawn field bar), found by `recognize_textbox_blanks`.
-- Longview Self Storage / T.C.'s RV rental agreement.
--
-- "Last:", "City:", "State:", "Zip:" and "Phone:" appear once per section
-- (occupant, alternate contact, lienholder), so each is anchored to text
-- that precedes only its own section. The occupant's "Address:" is already
-- covered by the existing anchored e.address row, so it is not repeated.
--
-- Not seeded on purpose: the lienholder section (no QMS tag for it), "Cell:"
-- (QMS has one phone tag, e.phone, already used by "Phone:"), "Employer:".
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('e.fname', 'label_proximity',
        '{"label": "Occupant Name - First:", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.lname', 'label_proximity',
        '{"label": "Last:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 150}}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.phone', 'label_proximity',
        '{"label": "_Phone:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 150}}',
        'Longview Self Storage / T.C.''s RV -- text-box label; the leading underscore is in the converted form''s own text'),
    ('e.post', 'label_proximity',
        '{"label": "Zip:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 150}}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.state', 'label_proximity',
        '{"label": "State:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 150}}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.city', 'label_proximity',
        '{"label": "City:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 150}}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.email', 'label_proximity',
        '{"label": "Email:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 150}}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.dob', 'label_proximity',
        '{"label": "D.O.B.", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.dlnum', 'label_proximity',
        '{"label": "Driver’s License:", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- text-box label'),
    ('e.a.phone', 'label_proximity',
        '{"label": "Phone:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "MAY BE SENT.", "within_chars": 100}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact; anchored on the end of the "ANOTHER PERSON ... MAY BE SENT." header, which only this section follows closely'),
    ('e.a.city', 'label_proximity',
        '{"label": "City:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "MAY BE SENT.", "within_chars": 100}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact'),
    ('e.a.address', 'label_proximity',
        '{"label": "Address:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "MAY BE SENT.", "within_chars": 100}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact'),
    ('e.a.state', 'label_proximity',
        '{"label": "State:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "MAY BE SENT.", "within_chars": 100}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact'),
    ('e.a.post', 'label_proximity',
        '{"label": "Zip:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "MAY BE SENT.", "within_chars": 100}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact');
