-- Generalizes vendor-format recognition (previously hardcoded per-tool --
-- Group Prep's unit-group/src/format.rs had its own QSX/Storage
-- Commander/DoorSwap consts, dedup had none at all and just hard-errored
-- on a missing FirtLast column) into one shared, DB-backed registry both
-- tools read from. Per Boris's explicit direction: prefer data over
-- hardcoded Rust wherever something is reasonably expected to grow or be
-- authored without a deploy — a vendor's header shape is exactly that
-- kind of fact, not logic. See core::vendor_format for the Rust side
-- (detect_vendor/apply_field_mapping now take this table's rows as
-- plain data, no vendor-specific branching there).
--
-- `content_type` distinguishes which tool's canonical-field pipeline a
-- row belongs to ('units' for Group Prep, 'tenants' for dedup) — plain
-- TEXT, not a Postgres enum, so a third content type later (or a third
-- tool) is a pure data addition, same reasoning VENDOR_FORMATS itself
-- used to justify no vendor-specific branching outside the format
-- module.
--
-- `field_mapping` is the literal (canonical target field, this vendor's
-- own source header) pairs, hand-authored per vendor exactly as
-- format.rs's default_mapping was — only now as data. A target with no
-- entry has nothing mapped and is dropped by apply_field_mapping, not
-- left blank (see the DoorSwap "Invalid dimensions on every row" bug
-- this exact rule was added to prevent).
--
-- `transform_key` is the one deliberate exception to "pure rename
-- mapping": a few vendors combine several canonical fields into one raw
-- column in a way no rename table can express (Easy Storage Solutions'
-- `Address` packs street + city/state/zip across an embedded newline).
-- Real parsing logic for those lives in core::vendor_format::transforms,
-- keyed by this column's value — deliberately NOT expressible from the
-- self-service "add a vendor" UI (there is no code field in this table),
-- so a custom vendor that turns out to need a real transform is a signal
-- it should graduate into a hand-authored, developer-reviewed row here
-- instead, the same trajectory Storage Commander/DoorSwap followed
-- before this table existed.
CREATE TABLE client_ops.vendor_format (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    content_type TEXT NOT NULL,
    signature_headers TEXT[] NOT NULL,
    field_mapping JSONB NOT NULL,
    transform_key TEXT,
    created_by UUID REFERENCES auth.users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (content_type, name)
);

ALTER TABLE client_ops.vendor_format ENABLE ROW LEVEL SECURITY;

-- Reference/business-config data, same posture as client_ops.qms_tag
-- post-widening: readable by any authenticated caller, mutable by the
-- three client-ops-adjacent roles (this is system configuration, not a
-- client operation itself, so it gets its own permission rather than
-- reusing client_ops.perform — same reasoning client_ops.manage_tags
-- was given).
CREATE POLICY vendor_format_select_authenticated ON client_ops.vendor_format
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY vendor_format_insert_client_ops_roles ON client_ops.vendor_format
    FOR INSERT
    WITH CHECK (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY vendor_format_update_client_ops_roles ON client_ops.vendor_format
    FOR UPDATE
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    )
    WITH CHECK (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY vendor_format_delete_client_ops_roles ON client_ops.vendor_format
    FOR DELETE
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );

INSERT INTO auth.permissions (key, label, description) VALUES
    ('client_ops.manage_vendor_formats', 'Manage vendor file formats', 'Add or edit the recognized vendor/PMS export formats (signature headers and field mappings) used by Group Prep and the duplicate-tenant check.');

INSERT INTO auth.role_permissions (role_id, permission_key)
SELECT r.id, 'client_ops.manage_vendor_formats' FROM auth.roles r
WHERE r.key IN ('admin', 'onboarding_manager', 'department_manager');

-- Seed: the vendors that were previously hardcoded in
-- unit-group/src/format.rs (content_type = 'units'), transcribed
-- verbatim from those consts, plus dedup's two tenant-file vendors
-- (content_type = 'tenants') -- QSX (previously the only shape dedup's
-- ingest.rs accepted, enforced as a hard "missing FirtLast column"
-- error) and Easy Storage Solutions (new, onboarding Affordable
-- Storage LLC - Thibodaux, LA, 2026-08-07 prelim data).
INSERT INTO client_ops.vendor_format (name, content_type, signature_headers, field_mapping) VALUES
('QSX', 'units', ARRAY['UnitGroup','Number','Category'], '[
    {"target":"Number","source":"Number"},
    {"target":"UnitGroup","source":"UnitGroup"},
    {"target":"Category","source":"Category"},
    {"target":"StandardRate","source":"StandardRate"},
    {"target":"Active","source":"Active"},
    {"target":"Damaged","source":"Damaged"},
    {"target":"Width","source":"Width"},
    {"target":"Length","source":"Length"},
    {"target":"Height","source":"Height"},
    {"target":"InsideOutside","source":"InsideOutside"},
    {"target":"Covered","source":"Covered"},
    {"target":"DoorType","source":"DoorType"},
    {"target":"DoorWidth","source":"DoorWidth"},
    {"target":"DoorHeight","source":"DoorHeight"},
    {"target":"NearElevator","source":"NearElevator"},
    {"target":"BottleCapacity","source":"BottleCapacity"},
    {"target":"Floor","source":"Floor"},
    {"target":"ClimateControlled","source":"ClimateControlled"},
    {"target":"Class","source":"Class"},
    {"target":"Power","source":"Power"},
    {"target":"Alarm","source":"Alarm"},
    {"target":"DriveUpAccess","source":"DriveUpAccess"},
    {"target":"Furnished","source":"Furnished"},
    {"target":"Lighting","source":"Lighting"},
    {"target":"Area","source":"Area"},
    {"target":"DoorCount","source":"DoorCount"},
    {"target":"ConversionType","source":"ConversionType"}
]'::jsonb),
('Storage Commander', 'units', ARRAY['UnitGroup','Number','Category','Locality'], '[
    {"target":"Number","source":"Number"},
    {"target":"UnitGroup","source":"UnitGroup"},
    {"target":"Category","source":"Category"},
    {"target":"StandardRate","source":"StandardRate"},
    {"target":"Active","source":"Active"},
    {"target":"Damaged","source":"Damaged"},
    {"target":"Width","source":"Width"},
    {"target":"Length","source":"Length"},
    {"target":"Height","source":"Height"},
    {"target":"InsideOutside","source":"Locality"},
    {"target":"Covered","source":"Covered"},
    {"target":"DoorType","source":"DoorType"},
    {"target":"DoorWidth","source":"DoorWidth"},
    {"target":"DoorHeight","source":"DoorHeight"},
    {"target":"NearElevator","source":"NearElevator"},
    {"target":"BottleCapacity","source":"BottleCapacity"},
    {"target":"Floor","source":"Floor"},
    {"target":"ClimateControlled","source":"ClimateControlled"},
    {"target":"Class","source":"Class"},
    {"target":"Power","source":"Power"},
    {"target":"Alarm","source":"Alarm"},
    {"target":"DriveUpAccess","source":"DriveUpAccess"},
    {"target":"Furnished","source":"Furnished"},
    {"target":"Lighting","source":"Lighting"},
    {"target":"Area","source":"Area"},
    {"target":"DoorCount","source":"DoorCount"},
    {"target":"ConversionType","source":"ConversionType"},
    {"target":"MonitoringEnabled","source":"MonitoringEnabled"},
    {"target":"SmartLockEnabled","source":"SmartLockEnabled"}
]'::jsonb),
('DoorSwap', 'units', ARRAY['Unit','Unit Type','Status','Customer'], '[
    {"target":"Number","source":"Unit"},
    {"target":"UnitGroup","source":"Unit Type"},
    {"target":"Status","source":"Status"},
    {"target":"Customer","source":"Customer"},
    {"target":"Phone","source":"Phone"},
    {"target":"Cell Phone","source":"Cell Phone"},
    {"target":"Email","source":"Email"},
    {"target":"Balance","source":"Balance"}
]'::jsonb),
('QSX', 'tenants', ARRAY['FirtLast','CustNumb','AddressStreet1'], '[
    {"target":"CustNumb","source":"CustNumb"},
    {"target":"UnitNumber","source":"UnitNumber"},
    {"target":"FirtLast","source":"FirtLast"},
    {"target":"FirstName","source":"FirstName"},
    {"target":"LastName","source":"LastName"},
    {"target":"CompanyName","source":"CompanyName"},
    {"target":"PhoneNumber","source":"PhoneNumber"},
    {"target":"PhoneNumberPrefix","source":"PhoneNumberPrefix"},
    {"target":"Email","source":"Email"},
    {"target":"AddressStreet1","source":"AddressStreet1"},
    {"target":"AddressStreet2","source":"AddressStreet2"},
    {"target":"AddressCity","source":"AddressCity"},
    {"target":"AddressState","source":"AddressState"},
    {"target":"AddressPostalCode","source":"AddressPostalCode"},
    {"target":"AlternateContactFirstName","source":"AlternateContactFirstName"},
    {"target":"AlternateContactLastName","source":"AlternateContactLastName"},
    {"target":"AlternateContactEmail","source":"AlternateContactEmail"},
    {"target":"AlternateContactPhoneNumber","source":"AlternateContactPhoneNumber"},
    {"target":"AlternateContactPhoneNumberPrefix","source":"AlternateContactPhoneNumberPrefix"},
    {"target":"AlternateContactAddressStreet1","source":"AlternateContactAddressStreet1"},
    {"target":"AlternateContactAddressStreet2","source":"AlternateContactAddressStreet2"},
    {"target":"AlternateContactAddressCity","source":"AlternateContactAddressCity"},
    {"target":"AlternateContactAddressState","source":"AlternateContactAddressState"},
    {"target":"AlternateContactAddressPostalCode","source":"AlternateContactAddressPostalCode"}
]'::jsonb);

-- Easy Storage Solutions gets its own INSERT (rather than folding into
-- the multi-row VALUES above) so the transform_key column — the one
-- real exception on this table — sits next to the row it actually
-- applies to, not buried in a shared column list every other row leaves
-- NULL.
--
-- AddressStreet1/City/State/PostalCode are mapped as plain identity
-- pairs here because split_ess_address (core::vendor_format::transforms)
-- runs BEFORE the generic rename step and writes those exact canonical
-- header names directly onto the document from ESS's single combined
-- `Address` column — apply_field_mapping itself stays 100% generic, the
-- vendor-specific part is contained entirely in the named transform.
-- CompanyName and AddressStreet2 have no ESS equivalent and are left
-- unmapped (dropped, not blank — see the DoorSwap dimensions bug this
-- rule prevents). AlternateContactFirstName takes ESS's single
-- `Alternate Contact` full-name field; AlternateContactLastName has
-- nothing to map and stays blank for every row, which is fine since
-- TenantRecord::display_name() and the comparison logic already
-- tolerate a blank name half.
INSERT INTO client_ops.vendor_format (name, content_type, signature_headers, field_mapping, transform_key) VALUES
('Easy Storage Solutions', 'tenants', ARRAY['Unit','Move-in Date','Tenant Protection'], '[
    {"target":"UnitNumber","source":"Unit"},
    {"target":"FirtLast","source":"Name"},
    {"target":"AlternateContactFirstName","source":"Alternate Contact"},
    {"target":"PhoneNumber","source":"Phone"},
    {"target":"Email","source":"Email"},
    {"target":"AlternateContactPhoneNumber","source":"Alternate Phone"},
    {"target":"AlternateContactAddressStreet1","source":"Alternate Address"},
    {"target":"AlternateContactAddressCity","source":"Alternate City"},
    {"target":"AlternateContactAddressState","source":"Alternate State"},
    {"target":"AlternateContactAddressPostalCode","source":"Alternate Zip"},
    {"target":"AddressStreet1","source":"AddressStreet1"},
    {"target":"AddressCity","source":"AddressCity"},
    {"target":"AddressState","source":"AddressState"},
    {"target":"AddressPostalCode","source":"AddressPostalCode"}
]'::jsonb, 'split_ess_address');
