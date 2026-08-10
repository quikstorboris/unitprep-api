DELETE FROM client_ops.qms_tag
 WHERE tag_key IN ('m.indate', 'm.secdep', 'l.indate', 'l.secdep', 'd.now', 'd.nowlong');
