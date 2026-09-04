ALTER TABLE clients.facility_policies
    DROP COLUMN fees_manually_exempt,
    DROP COLUMN taxes_manually_exempt,
    DROP COLUMN delinquency_manually_exempt,
    DROP COLUMN coverage_manually_exempt,
    DROP COLUMN specials_manually_exempt;
