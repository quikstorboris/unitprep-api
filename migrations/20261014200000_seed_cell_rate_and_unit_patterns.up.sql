-- Longview Self Storage / T.C.'s RV rental agreement, follow-up to the
-- underlined-tab and text-box pattern seeds: three more fields, each chosen
-- explicitly by Boris.
--   * "Cell:" -> e.phone. QMS has a single tenant phone tag, so this is the
--     same tag the occupant's "Phone:" box already uses.
--   * "Rental Rate Per Month" -> m.effrate. An m.* tag (move-in context),
--     approved here because this is the move-in lease itself.
--   * "LVSS" -> u.num. The form prints one unit-number blank per facility
--     ("LVSS <blank> TC'S <blank>"); this is the Longview Self Storage copy.
--     The "TC'S" blank is deliberately not seeded.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('e.phone', 'label_proximity',
        '{"label": "Cell:", "position": "after", "max_gap_chars": 5, "requires_preceding_anchor": {"text": "Occupant Name - First:", "within_chars": 300}}',
        'Longview Self Storage / T.C.''s RV -- text-box label for the occupant''s cell phone'),
    ('m.effrate', 'label_proximity',
        '{"label": "Rental Rate Per Month", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- underlined tab blank after "$"'),
    ('u.num', 'label_proximity',
        '{"label": "LVSS", "position": "after", "max_gap_chars": 5}',
        'Longview Self Storage / T.C.''s RV -- unit number blank for the Longview Self Storage facility');
