DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND tag_key IN ('e.address', 'e.a.address', 'e.email', 'e.a.email', 'e.a.phone')
   AND pattern ? 'requires_preceding_anchor';
