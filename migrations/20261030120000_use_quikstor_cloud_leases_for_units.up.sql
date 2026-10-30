-- QuikStor Cloud's Leases.csv links each tenant to the unit(s) they hold
-- (one row per lease: LegacyTenantId + UnitNumber). Until now it was only
-- recognized; findings named records by tenant ID because Tenants.csv has
-- no unit. It becomes a join file: dedup reads it together with the
-- tenants file and gives every record its real unit number (a tenant with
-- three leases becomes three records, as in a QMS export).
--
-- Mapped to dedup's canonical TenantId and UnitNumber only; the other
-- lease columns (balances, dates, ...) are not dedup inputs. A file that
-- carries a tenant ID and a unit but no customer name is treated as a
-- unit link by dedup/src/unit_link.rs rather than matched by name.
--
-- Verified on two real pulls (Davidson Road 1st prelim: 291 tenants, 344
-- leases; Freeland final: 107 tenants, 129 leases): every tenant has at
-- least one lease, no lease has a blank unit.

UPDATE client_ops.vendor_format
SET file_role = 'join',
    field_mapping = '[
        {"target":"TenantId","source":"LegacyTenantId"},
        {"target":"UnitNumber","source":"UnitNumber"}
    ]'::jsonb,
    guidance = 'One row per lease, linking each tenant (LegacyTenantId) to a unit (UnitNumber). Selected together with Tenants.csv, it gives every record its real unit number; a tenant with several leases becomes one record per unit.'
WHERE content_type = 'tenants' AND name = 'QuikStor Cloud Leases';

UPDATE client_ops.vendor_format
SET guidance = 'Tenants.csv from the QuikStor Cloud pull: the tenant name, contact details and address, but no unit numbers. With Leases.csv selected alongside it, every record gets its real unit number; without it, findings name records by tenant ID (LegacyTenantId). Units.csv has no tenant link and is not a dedup file.'
WHERE content_type = 'tenants'
  AND name IN ('QuikStor Cloud', 'QuikStor Cloud (street address headers)');
