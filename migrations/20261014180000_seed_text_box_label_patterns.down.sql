DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND notes LIKE 'Longview Self Storage / T.C.%'
   AND tag_key IN ('e.fname', 'e.lname', 'e.phone', 'e.post', 'e.state', 'e.city', 'e.email',
                   'e.dob', 'e.dlnum', 'e.a.phone', 'e.a.city', 'e.a.address', 'e.a.state',
                   'e.a.post');
