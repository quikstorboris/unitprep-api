DELETE FROM client_ops.qms_tag
 WHERE tag_key IN (
    'e.name', 'e.mil', 'e.init', 'e.comp', 'e.zip', 'e.keycodes', 'e.dlexp', 'e.alt', 'e.title', 'e.int',
    'e.a.name', 'e.a.add1', 'e.a.add2', 'e.a.phone', 'e.a.state', 'e.a.city', 'e.a.post', 'e.a.email',
    'e.a.address', 'e.a.zip', 'e.a.rel', 'e.a.fname', 'e.a.lname',
    'e.m.cophone', 'e.m.branch', 'e.m.colname', 'e.m.cofname', 'e.m.id', 'e.m.eserv', 'e.m.unit',
    'e.m.sserv', 'e.m.a.lname', 'e.m.a.fname',
    'f.name', 'f.add1', 'f.add2', 'f.state', 'f.city', 'f.post', 'f.phone', 'f.email', 'f.address',
    'f.porturl', 'f.ow.firstname', 'f.ow.lastname',
    'c.name', 'c.email',
    'u.dim', 'u.length', 'u.width', 'u.type', 'u.stdrate', 'u.area',
    'm.ptd', 'm.ptd+1', 'm.descgood', 'm.insprice', 'm.promo.name', 'm.maxra', 'm.ins', 'm.leadsrc', 'm.liens',
    'l.nxtamt', 'l.ptd+1', 'l.baldue', 'l.effrate',
    'm.vi.pn', 'm.vi.model', 'm.vi.make', 'm.vi.ps', 'm.vi.year', 'm.vi.vin', 'm.vi.lhfn', 'm.vi.lha',
    'm.vi.vt', 'm.vi.ipn', 'm.vi.note', 'm.vi.nor', 'm.vi.color', 'm.vi.ii',
    'l.vi.ps', 'l.vi.pn', 'l.vi.year', 'l.vi.color', 'l.vi.ied', 'l.vi.ii', 'l.vi.vin', 'l.vi.ipn',
    'l.vi.lha', 'l.vi.model', 'l.vi.make', 'l.vi.lhpn', 'l.vi.lhfn',
    'm.opi.lhfn', 'm.opi.lha', 'm.opi.desc', 'm.opi.lhpn',
    'sig'
 );
