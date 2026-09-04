//! The `clients` domain: mapping Process Street data into the `clients`
//! Postgres schema (see migration `20260828120000_create_process_street_client_tables`
//! and its follow-up `20260828130000_add_merchant_account_encrypted_pii`),
//! and the field-level encryption the New Merchant Account workflow's
//! sensitive data needs. Distinct from `process_street` (pure, read-only
//! PS API access -- knows nothing about this app's own schema) and from
//! `client_ops` (an unrelated, pre-existing domain: tool-support/
//! reference data for Group Prep/dedup/the template tagger, not the
//! clients themselves -- see the `clients` vs. `client_ops` schema
//! naming decision recorded in the vault and in this migration's own
//! header comment).

pub mod company_naming;
pub mod contract_order_mapping;
pub mod create;
pub mod encryption;
pub mod fields;
pub mod ingest;
pub mod intake_mapping;
pub mod known_workflows;
pub mod merchant_account_correlation;
pub mod merchant_account_mapping;
pub mod people;
pub mod person_index;
pub mod policy_exemption;
pub mod repository;
pub mod search;
pub mod sync;
