DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND (tag_key, pattern) IN (
       ('f.name', '{"label": "FROM:", "position": "after", "max_gap_chars": 30}'),
       ('e.name', '{"label": "TO:", "position": "after", "max_gap_chars": 30}'),
       ('u.num', '{"label": "Unit", "position": "after", "max_gap_chars": 10}')
   );
