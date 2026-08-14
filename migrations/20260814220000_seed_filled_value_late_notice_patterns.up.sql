-- New source document for the pattern corpus: a system-generated rent
-- late-notice letter (No Ka Oi Self Storage), the first real example
-- of an ALREADY-FILLED document run through the tagger rather than a
-- blank template -- the case recognize_filled_values (tagger-pipeline)
-- exists to handle. Every label below is real, measured against the
-- letter's actual flattened text (paragraphs joined by \n, matching
-- docx_surgeon::read_docx's own separator):
--
--   FROM: (label_end 6) -> "No Ka Oi Self Storage" ends at the next \n
--   (position 28) -- gap 22, max_gap_chars 30 for margin.
--   TO: (label_end 83) -> "ABM PARKING SERVICE" ends at the next \n
--   (position 103) -- gap 20, max_gap_chars 30 for margin.
--   Unit (label_end 220, the occurrence in "Dear ... # Unit 1000") ->
--   "1000" ends at the next \n (position 225) -- gap 5, max_gap_chars
--   10 for margin.
--
-- All three tags are within unitprep-tagger-template-tagger's locked
-- safe scope (name/address/phone/email/DL#/unit number) -- no m.*/l.*/
-- d.* context-dependent tag introduced here.
--
-- "Unit" also matches a second, generic occurrence later in the same
-- letter ("rent on your unit, Unit 1000, was due...") -- deliberately
-- not a problem this pattern needs to solve: recognize_filled_values
-- only proposes a value when a real boundary (paragraph break, tab, or
-- another label) sits within max_gap_chars, so the generic mid-sentence
-- mention correctly produces no candidate rather than a wrong, guessed
-- one -- see that function's own doc comment.
INSERT INTO client_ops.tag_pattern (tag_key, kind, pattern, notes) VALUES
    ('f.name', 'label_proximity',
        '{"label": "FROM:", "position": "after", "max_gap_chars": 30}',
        'No Ka Oi Self Storage late-notice letter'),
    ('e.name', 'label_proximity',
        '{"label": "TO:", "position": "after", "max_gap_chars": 30}',
        'No Ka Oi Self Storage late-notice letter'),
    ('u.num', 'label_proximity',
        '{"label": "Unit", "position": "after", "max_gap_chars": 10}',
        'No Ka Oi Self Storage late-notice letter -- "Dear ... # Unit 1000" occurrence only; a later generic "your unit," mention in the same letter does not produce a false match, see this migration''s own note');
