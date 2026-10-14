-- Label patterns for the Longview Self Storage / T.C.'s RV rental agreement,
-- a PDF-converted form whose fill-in lines are underlined tab stops (no
-- literal underscores) and whose initials boxes are empty table cells.
-- Recognized by `recognize_tab_blanks` and the pipeline's empty-cell rule,
-- both of which read these same label_proximity rows.
--
-- Not seeded on purpose (see the vault's Template Tagger notes):
--   * the unit number -- this form prints one blank per facility
--     ("LVSS <blank> TC'S <blank>"), so which one is `u.num` depends on
--     which facility's copy of the template this is;
--   * the monthly rate (`m.effrate`) -- locked with the other m.*/l.*/d.*
--     context-dependent tags until a disambiguation mechanism exists.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('m.indate', 'label_proximity',
        '{"label": "DATE OF LEASE", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- "entered into ... as of the date set forth above"; same slot Stowaway and Rest Stop tag as m.indate (the signing document''s own date), not d.now'),
    ('e.keycodes', 'label_proximity',
        '{"label": "Access Code", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- underlined tab blank'),
    ('e.init', 'label_proximity',
        '{"label": "initials here", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- "By placing his/her initials here," | <empty table cell> | ", Occupant acknowledges ..."'),
    ('e.a.name', 'label_proximity',
        '{"label": "Name:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "ANOTHER PERSON", "within_chars": 350}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact, after "PROVIDE THE NAME AND ADDRESS OF ANOTHER PERSON"'),
    ('e.a.email', 'label_proximity',
        '{"label": "Email:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "ANOTHER PERSON", "within_chars": 700}}',
        'Longview Self Storage / T.C.''s RV -- alternate contact');
