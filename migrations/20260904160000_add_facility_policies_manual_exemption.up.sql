-- Splitting the single "Facility Policies" tab into five (Fees/Taxes/
-- Delinquency/Coverage/Specials) means each becomes independently
-- editable -- the first time this app has any editable field at all.
-- For a QSX-legacy facility (nothing came from Process Street for a
-- given category because QSX itself has no equivalent PS step), a
-- manager can now type that category in by hand. These five flags mark
-- a category permanently exempt from any future policy-refresh sync
-- once that happens -- set once, at the moment of that category's first
-- manual edit, and only when the category was empty at that moment on a
-- facility whose `previous_pms` names QSX (see
-- `clients::policy_exemption`'s own module doc for the exact detection
-- and the reasoning against ever un-setting this flag automatically).
--
-- Not needed for a category that DID come from Process Street: a manual
-- correction there stays subject to the normal per-field conflict
-- resolution the "Re-sync" screen already uses for scalar company/
-- facility fields (`manually_edited_fields`) -- this table's own flags
-- are strictly the QSX-only, empty-at-ingest, permanent case.
ALTER TABLE clients.facility_policies
    ADD COLUMN fees_manually_exempt BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN taxes_manually_exempt BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN delinquency_manually_exempt BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN coverage_manually_exempt BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN specials_manually_exempt BOOLEAN NOT NULL DEFAULT false;
