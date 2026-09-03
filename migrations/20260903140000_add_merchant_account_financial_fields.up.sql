-- Elavon tab (Facility page): the revenue/volume fields New Merchant
-- Account's Facility Information (Pre-App) step captures, confirmed
-- genuinely per-facility (Prairie Enterprises' 3 real facilities each
-- answered these differently on their own separate runs -- there is no
-- single "company-wide" revenue figure to show on the Company page
-- instead). Already present in `raw_ps_snapshot` as unstructured JSON
-- since these fields were never in `merchant_account_mapping`'s
-- sensitive denylist, but never promoted to named columns or shown in
-- any UI until now. Kept as raw text, not decimals -- same Phase 1
-- convention as every other PS-sourced financial field in this schema
-- (policy_fees.raw_value, policy_taxes.*_raw, etc.).
ALTER TABLE clients.facility_merchant_accounts
    ADD COLUMN total_annual_business_revenue_raw TEXT,
    ADD COLUMN total_monthly_sales_raw TEXT,
    ADD COLUMN average_credit_card_payment_amount_raw TEXT,
    ADD COLUMN highest_credit_card_payment_amount_raw TEXT,
    ADD COLUMN high_cc_payment_times_per_year_raw TEXT,
    ADD COLUMN offers_ach_raw TEXT,
    ADD COLUMN annual_electronic_check_volume_raw TEXT,
    ADD COLUMN average_electronic_check_amount_raw TEXT,
    ADD COLUMN maximum_electronic_check_amount_raw TEXT;
