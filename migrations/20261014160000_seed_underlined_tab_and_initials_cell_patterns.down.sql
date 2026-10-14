DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND notes LIKE 'Longview Self Storage / T.C.%'
   AND tag_key IN ('m.indate', 'e.keycodes', 'e.init', 'e.a.name', 'e.a.email');
