-- Persists a Merchant Account run's own `Business_DBA` form field (see
-- merchant_account_correlation.rs's own doc comment for why) so
-- correlate_by_title can match against it without a live PS call --
-- same "purely off already-locally-indexed data" constraint every
-- other correlation signal already respects. Populated by the sync
-- orchestrator's own sync_one_run, which already fetches this run's
-- form fields for ps_person_index -- no extra network call needed.
--
-- Nullable, and NOT restricted to workflow = 'merchant_account': a
-- future workflow could carry an equally-useful identifying field
-- under this same generic column without another migration; today,
-- intake/contract_order runs simply never populate it since their own
-- forms have no Business_DBA field to extract.
ALTER TABLE clients.ps_sync_state
    ADD COLUMN business_dba TEXT;
