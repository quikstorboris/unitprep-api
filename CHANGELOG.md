# Changelog

All notable changes to `unitprep-api` are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.9.118] - 2026-10-09

ClickUp run updates for Unit Groups and the Template Tagger (task phrases and comment wording are data), and a facility "Last Synced Project" log. **Migration `20261009120000`** (prod: `scripts/prod_db_status.sh`, then `prod_db_sync.sh`, before this release goes live).

### Added
- **Unit Groups and Template Tagger runs can update their ClickUp task** the way a duplicate check does: find the task in the facility's linked list (the person confirms it), comment with a link to the saved results file, add the person as assignee, set the task complete. The tasks are "CONFIGURE Unit Setup" (Unit Groups) and "APPLY TAGS to Lease" (Template Tagger). A later run of either only adds a comment.
- `GET /clients/{id}/facilities/{id}/clickup/sync-log` (`clickup_copy::sync_log`): the "Last Synced Project" log -- every comment copy made onto the facility (source facility, who, when, copied/failed/completed counts, dialog or bulk), newest first, keyset-paged with `before_id`/`limit`. It reads the existing `facility_clickup_comments_copied` rows in the Activity Logs trail instead of keeping a second table that could disagree with it. Needs `integrations.clickup` (and, through RLS, a client-ops role to see rows).
- Audit event `facility_clickup_run_posted` for non-dedup runs posted to ClickUp (`metadata.tool`, `metadata.step`); duplicate checks keep `facility_clickup_duplicate_check_posted`.

### Changed
- **Task phrases and comment wording are data, not constants.** `integrations.clickup_task_steps` gains `tool`, `comment_lead`, `comment_link_text`, `comment_without_link` and `comment_only_from_sequence` (and `UNIQUE (tool, ordinal)`); the duplicate-check wording that was in Rust constants is now in its two rows, and the two new steps are seeded. A run is matched to the step with the highest `ordinal` not above its position among the facility's runs of that tool. Editing a row changes what is matched and said with no deploy; a settings screen for it is not built yet.
- `clickup_duplicate_check` is now `clickup_run_update`, and its routes `clickup/duplicate-check-tasks` / `duplicate-check-results` are `clickup/run-tasks` / `run-results` (the UI moves with them). A run with no step row answers 409 `clickup_step_not_configured`.
- `SCHEMA.sql` regenerated (111 migrations).

### Tests and CI
- Test only: a race that made the ClickUp real-DB tests fail about one run in six when all `_db_` tests ran together. Every test that depends on `INTEGRATION_SECRETS_ENCRYPTION_KEY` is now in the existing serial group, so one test removing the key can no longer land between another test setting it and decrypting its token. No production code changed.
- `scripts/run_ci_db_tests.sh` now also runs every ignored test whose name contains `_db_` (103 tests, about 8 s) on top of its 18-name allowlist, so the refactor safety nets (sessions, resync/sync, registration, audit rows, ClickUp) run in CI. See the script header for the convention and the trade-off.
- Test only: `concurrent_load_report` (`src/api/concurrent_load_tests.rs`, ignored), a report-only benchmark that drives the real router with concurrent dedup operators and prints latency percentiles, throughput and peak pool use. See its module doc for the command and knobs. No production code changed.

## [1.9.117] - 2026-10-09

ClickUp Copy offers every Onboarding Phase, not only Set Up and Migration. No migration.

### Changed
- **ClickUp Copy now includes every phase the list has** -- Scheduling, Show Stoppers, Training and the rest -- instead of the two hard-coded ones (`COPY_PHASES` is gone). Any task with an Onboarding Phase is offered and paired within its own phase; a task with no phase is still not offered. The phases are whatever options the list's field defines, so a new phase in the template needs no code change.
- `copy-pairs` rows and `bulk-tasks` tasks carry `phase_order` (the phase's position in the field's own option list), so the pages show the groups in the ClickUp list's order. Tasks now remember their dropdown option's `orderindex` (`TaskDropdown.option_order`).
- Tests: the mock ClickUp gains Scheduling and Show Stoppers tasks and a phase-less one; 3 new pairing unit tests (every phase offered, a phase-less task not offered, phase order).

## [1.9.116] - 2026-10-08

Efficiency refactor chunk F7f (api side): the company-list, resync, manual-link, directory filter, client-import preview/create, search, onboarding-summary and a few auth/settings types are exported to TypeScript by ts-rs. No behaviour change; no change to any JSON.

### Added
- `ts_rs::TS` + `#[ts(export)]` on `CompanySummary` / `StaffRef` (`clients_companies.rs`); `ResyncConflict`, `PreviewResyncResponse`, `ConflictResolution`, `ApplyResyncResponse` (`clients_resync/`); `ManualLinkRequest` / `ManualLinkResponse` / `ManualLinkWorkflow`; `FilterOptionsResponse` / `StateOption`; `MappedCompany`, `MappedFacility` (`clients/intake_mapping.rs`), `PreviewedRun`, `PreviewClientsResponse`, `CreateClientRequest`, `CreateFacilitySelection`, `CreateClientResponse`, `EditableFacilityFields`; the search DTOs (`DuplicateCandidate`, `FacilityMatch`, `PersonMatch`, `MerchantAccountMatch`, `SearchClientsResponse`, `MatchedVia`); `FacilityOnboardingSummary` / `OnboardingSummaryResponse`; `RoleInfo`, `GrantablePermission`, `UserPermissionsResponse`, `CreateInviteResponse`; and `ConfigSource`. `scripts/check_ts_bindings.sh` now covers 100 files.
- Two field overrides so the generated type is as precise as the hand-written one it replaces: `PersonMatch.workflow` is typed as the `"intake" | "merchant_account" | "contract_order"` union, and the `i64` `FacilityOnboardingSummary.duplicate_checks_completed` as `number` (ts-rs defaults to `bigint`).

### Notes
- Deliberately NOT generated: `UserSummary` and `CreateInviteRequest` (the UI narrows `role` / `company` to unions the Rust `String` fields cannot express), `SyncStatus` (`state` is a bare `&'static str` in Rust; the UI's union is tighter), and the tool-run summary union (its Rust side builds the report summary as an untyped JSON value).

## [1.9.115] - 2026-10-08

Efficiency refactor chunk A6b: the Template Tagger's CPU-bound steps run on the blocking pool. No behaviour change.

### Changed
- `/tagger/check` (and the Dropbox import that shares it), `/tagger/report` and `/tagger/apply` parsed the .docx, matched patterns, built candidate views, created the session and rewrote the .docx directly on async worker threads. Each of those steps now goes through `run_blocking` with a closure that owns its data (`tagger_read_docx`, `tagger_find_candidates`, `tagger_create_session`, `tagger_report`, `tagger_apply_read_docx`, `tagger_edit_docx`), the same pattern as the dedup and Unit Groups handlers (A4-A6). Control flow, error responses and logging are unchanged, including the `too_many_candidates` rejection; the 23 existing Tagger tests pass untouched. Templates are small, so this is hygiene rather than a measured win.

## [1.9.114] - 2026-10-08

Efficiency refactor chunk F7e (api side): the company-detail, facility-people and Elavon response types are exported to TypeScript by ts-rs. No behaviour change; no change to any JSON.

### Added
- `ts_rs::TS` + `#[ts(export)]` on `CompanyDetailResponse`, `FacilitySummary`, `OwnerInfo`, `ClickUpParentChange` (`api/clients_detail/company/dto.rs`); `FacilityPeopleResponse`, `FacilityPerson`, `LegalOwnerSource`, `MissingLegalOwner` (`api/clients_facility_people/dto.rs`) and `PersonAssignment` (`clients/people.rs`); and `ElavonStatusResponse` (the `status`-tagged enum), `ElavonPartyInfo`, `ElavonFinancials`, `ElavonCandidate`, `ElavonQmsCredentials`, `ElavonPinpadCredentials` (`api/clients_elavon/dto.rs`). `scripts/check_ts_bindings.sh` now covers 68 files. The generated shapes matched the hand-written UI types exactly.

## [1.9.113] - 2026-10-08

Efficiency refactor chunk F7d (api side): the facility and policy response types are exported to TypeScript by ts-rs. No behaviour change; no change to any JSON.

### Added
- `ts_rs::TS` + `#[ts(export)]` on `FacilityDetailResponse` and on the policy types `FeeRow`, `TaxesRow`, `DelinquencyStepRow`, `TaxEntryRow`, `DelinquencyEntryRow`, `CoverageTierRow`, `CommissionRow`, `FacilityPoliciesResponse` (`api/clients_detail`) and the two inputs the UI re-uses as types, `TaxEntryInput` and `DelinquencyEntryInput` (`api/clients_facility_policies_edit`). `scripts/check_ts_bindings.sh` now covers 53 files. The `i64` row ids carry `#[ts(type = "number")]` (ts-rs would otherwise type them `bigint`; they are small JSON numbers).
- The workspace `ts-rs` dependency enables the `uuid-impl` and `chrono-impl` features, so `Uuid` and chrono dates export as `string` (their JSON form) without a per-field override -- needed by every remaining domain (company, directory, search, import, settings). No new crates; `Cargo.lock` gains two dependency edges.

## [1.9.112] - 2026-10-08

Refactor chunk G1b (operational logging and audit gaps). No migration; no change to any JSON.

### Added
- **Bootstrap is on the record.** `bootstrap-admin` (create the first administrator, or `--reissue-invite`) now writes an `invite_created` security-trail row: no actor (the operator has no application identity), the account as target, `metadata` = `{via: "bootstrap_cli", mode: "create_administrator" | "reissue_invite", invite_hours}`, never the token. Smoke-tested end to end against a scratch copy of the test database with the real binary: both modes write their row, and no token-like value appears in any row.
- **Pool exhaustion is its own log line.** When a handler cannot get a connection before the acquire timeout, `begin_for` now logs `database connection pool exhausted` with `pool_size` and `pool_idle` (it used to be a generic "failed to open the request's RLS transaction" with the sqlx error text).
- **Retry give-ups are logged.** `integrations::http::send_with_retry` logs one `warn` when it gives up after retrying (still failing / still unreachable / upstream asked for a longer `Retry-After` than allowed) -- previously only the individual retries were logged and the end result showed up only as a handler error.
- **More slow-operation warnings.** `warn_if_slow` (2 s) now also covers dedup export (download and Dropbox), Template Tagger check and apply, and Unit Groups validate, analyze and export, next to the three dedup handlers that already had it.

### Changed
- `Debug` for `UpdateDropboxSettingsRequest`, `UpdateProcessStreetSettingsRequest`, `SaveTokenRequest` and `DecryptedElavonCredentials` is hand-written and prints `<redacted>` for the secrets (app secret, refresh token, API key, ClickUp token, QSS PINs), so a future `tracing::..!(request = ?request)` cannot leak them. An audit of every `tracing` call found no current log of a request body, headers or credential value (upstream response bodies are truncated since A2); this closes the one way that could regress. Tests print each type and assert the secret is absent and the non-secret fields are present.

## [1.9.111] - 2026-10-08

Refactor chunk G1a (audit-log gaps): admin edits of integration settings and exports of the activity log are now on the record. One new event in each trail; no migration, no change to any existing JSON.

### Added
- **`integration_settings_updated`** (security trail, `auth.auth_audit_logs`): written after the commit when an admin saves the Dropbox settings, the Process Street settings, or a Process Street task-role mapping. `metadata.integration` is `dropbox` / `process_street` / `process_street_task_roles`; `metadata.details` records what was set (the Dropbox root path and namespace, the Process Street schedule, the role and its task names) and ONLY that a secret was replaced (`app_secret_replaced`, `refresh_token_replaced`, `api_key_replaced`) -- never the value. The caller's IP is recorded. A refused or rejected request writes nothing. Previously none of these three admin credential/config changes left any audit row.
- **`activity_log_exported`** (client-ops trail, `client_ops.audit_log`): written when someone exports the activity log as a PDF, with the filters used and the row count -- the counterpart of the security trail's existing `audit_log_exported`. Both new events are in their trail's `ALL` list, so the admin filter dropdowns offer them with no frontend change beyond the Security Logs category preset.
- `api/integration_settings_audit.rs`, the one place the settings row shape is defined.
- Six real-DB tests (`integration_settings_audit_db_tests.rs`): each handler writes its row with the right actor / integration / IP; the Dropbox and Process Street rows are checked not to contain the secret that was just saved; a rejected task-role or schedule edit writes no row; the activity-log export writes its row. Mutation-checked: breaking the event name fails the three positive settings tests.

### Changed
- `dropbox_settings::update_settings`, `process_street_settings::update_settings` and `process_street_task_roles::update_task_role` take a `ConnectInfo<SocketAddr>` (as the other audited handlers do) so the row carries the client IP, and pass it to the permission check as well; their tests and the route-manifest gate test were updated for the extra argument.

## [1.9.110] - 2026-10-08

Efficiency refactor chunk F7c (api side): the dedup file-classification types are exported to TypeScript by ts-rs. No behaviour change; no change to any JSON.

### Added
- `ts_rs::TS` + `#[ts(export)]` on `ClassifiedFileView`, `SuggestionView`, `ClassifyResponse`, `ClassifyFileInput`, `RequirementFormat`, `RequirementVendor` and `FileRequirementsResponse` (`api/dedup_files.rs`) and on `FileRole` / `FileStatus` (`unitprep-dedup`'s `file_selection.rs`). `SuggestionView.alternatives` (a `BTreeMap`) carries `#[ts(type = "Record<string, string>")]`: ts-rs would otherwise type its values as possibly-absent. `scripts/check_ts_bindings.sh` now covers 42 files.

## [1.9.109] - 2026-10-08

Efficiency refactor chunk F7b (api side): the Template Tagger check/apply types are exported to TypeScript by ts-rs. No behaviour change; no change to any JSON.

### Added
- `ts_rs::TS` + `#[ts(export)]` on `TaggerCheckResponse`, `CandidateView`, `RegionView`, `TierView` (`api/tagger/views.rs`) and `ConfirmedSubstitution` (`api/tagger/apply.rs`, a request-only type). `scripts/check_ts_bindings.sh` now covers 33 files. The generated shapes matched the hand-written UI ones exactly. `TaggerRunSummary` stays hand-mirrored: `check_summary` builds it as a `serde_json::Value`, so there is no struct to derive from (making it one is a separate change).

## [1.9.108] - 2026-10-08

Dev tooling only: the `api-dev` watcher no longer writes TypeScript bindings into the bind-mounted checkout. No change to the service.

### Fixed
- `docker-compose.yml`: `api-dev` sets `TS_RS_EXPORT_DIR=/tmp/ts-bindings`. The container runs its test watcher as root, and ts-rs writes `export_bindings_*` output as a side effect of those tests; at the default (`./bindings` per crate) that created root-owned files and directories in the host's checkout, after which any host-side `cargo test` or `scripts/preflight.sh` failed every `export_bindings_*` test with `PermissionDenied`. Host runs are unaffected, and `npm run generate-types` / `scripts/check_ts_bindings.sh` keep passing their own `TS_RS_EXPORT_DIR`, so nothing about how bindings reach `unitprep-ui` changes. Takes effect when the container is recreated (`docker compose --profile dev up -d api-dev`).

## [1.9.107] - 2026-10-08

Efficiency refactor chunk F7a (api side): the dedup report views are exported to TypeScript by ts-rs. No behaviour change; no change to any JSON.

### Added
- `#[derive(ts_rs::TS)]` + `#[ts(export)]` on `DedupCheckResponse`, `DedupReportView` and every view under it (`dedup_view.rs`), and on the enums they reference in `unitprep-dedup` (`FieldCategory`, `FieldName`, `RelatednessSignal`, `UnidentifiedMode`). `unitprep-dedup` gains the `ts-rs` dependency (already a workspace dependency; a data-shape-only derive, like its existing `serde` one). `scripts/check_ts_bindings.sh` (preflight step 9) now covers 28 files instead of 11, so a backend rename of any of these fields fails the gate until `unitprep-ui` is regenerated.

### Notes
- Regenerating found one real mismatch: the hand-written UI `FieldName` union listed `PhoneNumberPrefix` and `AltContactPhoneNumberPrefix`, which the Rust enum deliberately excludes (see its doc comment) and the backend never sends. The generated type drops them; nothing in the UI referenced them.
- `duplicate_customer_records` and `unidentified` were optional in the hand-written UI type ("reports cached before this field existed"); the generated type makes them required, because the backend always sends them. UI code that tolerates their absence still compiles and still protects against old stored reports.

## [1.9.106] - 2026-10-08

Efficiency refactor chunk D5a: the `Database`/`Environment` marker both integration settings pages report is one type. No behaviour change.

### Changed
- `ConfigSource` was declared identically in `api/dropbox_settings.rs` and `api/process_street_settings.rs`; it now lives in `integrations/config_source.rs` and both import it (same JSON: `"database"` / `"environment"`). Deliberately not unified further: the two settings handlers differ in what they store (Dropbox: five fields, Process Street: a schedule plus an API key) and ClickUp credentials are per-user with a per-user AAD, so a generic settings skeleton would force three shapes into one for little gain.

## [1.9.105] - 2026-10-08

Efficiency refactor chunk D5b: known-answer tests for the three encryption formats. Tests only, plus one test-lock rename.

### Added
- Known-answer ("golden") tests for the TOTP secret, client PII and integration-secret blobs: a blob produced once by the shipped code (fixed key, fixed AAD, fixed plaintext) is hard-coded and must still decrypt, must not open under another AAD (or user), and must not open under another key. Round-trip tests alone pass even if a change alters the AAD or byte layout on both sides at once, which would make every stored secret unreadable; these do not.
- `integrations::secrets` had no negative tests for its key handling: wrong-length key (message names the length), non-hex key, unknown format version and too-short blob (both refused BEFORE the key is read, as documented).

### Fixed
- Two `tool_runs` tests that set `CLIENT_PII_ENCRYPTION_KEY` used the serial lock `client_pii_env` while every other test touching that variable used `client_pii_encryption_key_env`; different locks meant they could run in parallel and race. Unified.

## [1.9.104] - 2026-10-08

Efficiency refactor chunk D2d (reduced form): the per-handler permission check is one line. No behaviour change.

### Changed
- 69 handlers in 38 files spelled the permission gate as `if let Err(response) = user.require_permission(..).await { return response; }`; each is now `try_response!(user.require_permission(..).await);`. The check, its audit row and its 403 are untouched; only the early-return plumbing is shared. About 110 lines removed.
- NOT done: a manifest-driven permission layer that would replace these calls. See the refactor note: the route manifest and the permission-gate tests already prove, for every gated route, that the handler refuses an unprivileged caller with the declared action; a layer would swap that proven enforcement for a new one, move where a refusal is audited, and require rewriting those tests to go through the router, for no reduction in what the database still enforces underneath.

## [1.9.103] - 2026-10-08

Efficiency refactor chunks D2b and D2c: one place builds error responses, one place clamps list limits. No behaviour change.

### Changed
- `api::error_response(status, code, message)` is now the only place that builds the `{error, message}` JSON error body; `bad_request`, `not_found`, `conflict`, `internal_error`, `session_not_found`, `stage_conflict`, the two "not configured" 503s, the rate-limit 429 and the body-rejection mapper all go through it. 74 hand-written `(StatusCode::X, Json(ApiErrorBody { .. })).into_response()` literals in 32 files became calls to the named helper for their status (`bad_request` 42, `conflict` 8, `not_found` 4) or to `error_response` (the other statuses). `ApiErrorBody` is now constructed in exactly one function. Handlers keep returning `Response` (hundreds of tests call them directly and read `.status()`), so this is a helper, not a `Result<_, ApiError>` return type.
- `api::paging::clamp_limit(requested, default, max)` replaces the three copies of `limit.unwrap_or(DEFAULT).clamp(1, MAX)` in the audit-log, activity-log and tool-run lists, with four unit tests.

## [1.9.102] - 2026-10-08

Efficiency refactor chunk D2a: opening the caller's RLS transaction is one line. No behaviour change.

### Changed
- New `api/rls.rs`: `begin_for(&state, &user, "Could not load X")` opens the caller's row-level-security transaction (`begin_rls_transaction` with their id and roles) and, on failure, logs the cause with the caller's id and answers the same 500; `try_response!` unwraps its `Result<_, Response>` into the handler's early return.
- 73 handlers in 45 files had the identical seven-line `match begin_rls_transaction(...) { Ok(tx) => tx, Err(err) => { tracing::error!(...); return internal_error("..."); } }`; each is now `let mut tx = try_response!(begin_for(&state, &user, "...").await);`. About 300 lines removed. The failure log line is now `failed to open the request's RLS transaction` with the caller-facing message as its `context` field (it used to be a per-endpoint sentence); the response is unchanged.
- Not changed: the other 59 `begin_rls_transaction` calls, which sit in helpers that return `Result`, return a cookie-jar tuple, or take a different identity (pre-authentication flows) -- they do not have the handler's early-return shape.

## [1.9.101] - 2026-10-07

Efficiency refactor chunk D4h (long functions, first batch): four of the longest non-route functions broken into named steps. No behaviour change.

### Changed
- `api::analyze::analyze` (303 lines) is now `read_inputs` (stage and group-file checks under the read lock), `compute` (the batch build and analysis on the blocking pool), `record_on_session` (the generation-guarded write-back) and `record_tool_run`, with `analyze` itself the ~60-line sequence. The stale-write-back race tests pass unchanged.
- `api::upload::upload` (194): the multipart reading loop is `read_upload`.
- `api::resolve_unit_format::resolve_unit_format` (213): the session mutation is `apply_resolution` and the error-to-response table is `not_ready_response`.
- `bootstrap::run` (191): `connect`, `create_administrator`, `retire_old_invites` (the `--reissue-invite` path) and `create_invite`; the command was driven end to end against the test database (reissue retires old invites and leaves one live; create refuses a non-empty database; an unknown account is refused).
- The remaining long functions are mostly handlers whose length is the RLS-transaction / permission-gate / error-response boilerplate that D2 removes; they are re-measured after D2 rather than split by hand now.

## [1.9.100] - 2026-10-07

Efficiency refactor chunk D4c (shared session IO): the file-download response and the save-location answer, copied across the three tools and four exports, are now one module. No behaviour change.

### Changed
- New `api/session_io.rs`: `attachment_headers` / `attachment_response` (content type plus `Content-Disposition: attachment`) replace the seven hand-built copies in the Duplicate Check export, the Tagger apply, the Group Prep ZIP, tool-run downloads, the users CSV and the two PDF logs; `SaveLocationResponse::next_to` replaces three identical `*SaveLocationResponse` structs and the three `format!("{folder}/{NAME}")` blocks in the Duplicate Check, Tagger and Group Prep `save_location` handlers. About 95 lines removed.
- Deliberately NOT merged: the three Dropbox import handlers and `first_uploaded_file` / `all_uploaded_files`. They look alike but take different inputs (a folder of files versus one template versus a unit-group pair), write different session types and different audit shapes; a shared abstraction would need a trait over all three and would hide more than it saves.

## [1.9.99] - 2026-10-07

Efficiency refactor chunk D4i: the Dropbox client gets one JSON-call helper and is split; the sync schedule and the Merchant Account secrets move out of their large files. No behaviour change.

### Changed
- `dropbox/client.rs` (1,406 lines) became `dropbox/client/`: `mod` (the client, token refresh, the 401 refresh-and-retry, and the new private `rpc` helper), `dto` (response shapes and `DropboxError`), `folders` (list, search, shared-link resolution, folder creation), `files` (download, upload, shared links), `tests`, `live_tests` (the `#[ignore]`d real-account tests). `rpc` replaces seven copies of the same "build request with token and path-root header, send with retry, read status and body" sequence; `Reply::parse` and `Reply::into_error` replace the repeated error mapping. The upload path is still deliberately not retried.
- `clients/sync/orchestrator.rs` (1,555 lines): the schedule (`ScheduleConfig`, `next_daily_occurrence`, `sleep_until_next_scheduled_sync`, `start_background_sync_task`) moved to `orchestrator/schedule.rs`; the three inline test modules became `tests`, `live_tests` and `batch_tests` files.
- `clients/merchant_account_mapping.rs` (940 lines) became `merchant_account_mapping/`: `mapping` (field mapping and parties) and `secrets` (sealing and decrypting facility secrets and party PII, masking). The facility-secrets and Elavon-credentials AAD (the facility id) is untouched -- the two are told apart only by plaintext shape, and changing the AAD would break stored data.

### Added
- Six hermetic mock-Dropbox tests for the calls that used to have only real-account tests: folder search (folders only, scoped to the root, namespace header), shared-link resolution (the two-step bridge; a file link or a failure degrades to `None`), "folder already exists" counting as success, and a new-or-existing shared link; plus a failed listing surfacing its status and body.

## [1.9.98] - 2026-10-07

Efficiency refactor chunk D4h (first part): three more large files split, two row tuples named. No behaviour change.

### Changed
- `api/auth_invites.rs` (1,085 lines) became `auth_invites/`: `invite` (create/re-issue an invite), `recovery` (admin account recovery), `tests`.
- `api/clients_facility_people.rs` (945) became `clients_facility_people/`: `dto`, `owners` (where a facility's legal owners come from), `get`, `add`, `edit`, `unlink`, `tests`.
- `clients/repository.rs` (815) became `clients/repository/`: `facility`, `people`, `merchant_account`, `contract_order`, `task_status`, `tests` (its 97 real-database tests pass unchanged).
- `resolve_session`'s six-element tuple is now `SessionRow`, and `DropboxConfig::from_db`'s five-element tuple a private `ConfigRow` (two `type_complexity` allows gone).
- Deliberately NOT changed: the `too_many_arguments` allows on `edit_person_and_facility_link` (its own comment records why a parameter struct was rejected: eight independent fields of one form submission, three call sites) and on `client_ops::audit_log::record` (one stable nine-argument shape used identically at 73 call sites; a struct would only rename positional arguments).

## [1.9.97] - 2026-10-07

Efficiency refactor chunk D4c (split half; the v1.9.96 tag points at an incomplete commit that does not compile - this release is the real one): `dedup.rs` (988 lines) and `tagger.rs` (947 lines) split into modules. No behaviour change.

### Changed
- `api/dedup.rs` became `dedup/`: `dto`, `upload` (`/dedup/check`), `import_dropbox`, `report`, `export` (save location and download), `export_dropbox`, `export_bytes` (CSV/XLSX/ZIP builders, file names, the download response), `session`, `tests`. `file_response`, `generate_export` and `ExportFormat` keep their `api::dedup::` paths for the callers that use them.
- `api/tagger.rs` became `tagger/`: `views`, `files`, `patterns`, `recognize` (check, Dropbox import, session creation), `report`, `apply` (substitutions and the edited `.docx`), `dropbox`, `tests`.
- Not done here: sharing the session-IO helpers the two tools duplicate (`first_uploaded_file`, `save_location`, the Dropbox import/save pairs, `file_response`) - that needs a design pass, tracked as the remaining part of D4c.

## [1.9.95] - 2026-10-07

ClickUp Copy can also complete the destination tasks (opt-in), and passkey registration is split into a module under new real-database tests. No migration.

### Changed
- `api/auth_register.rs` (1,102 lines) became `auth_register/`: `dto`, `responses`, `begin`, `finish`, `enrol` and `tests` (refactor chunk D4g). No behaviour change: the characterization tests below pass unchanged.

### Added
- Real-database characterization tests for passkey registration (`auth_register_db_tests`), written before the split: the invite path, failed verification, mid-ceremony expiry, credential-insert failure, invite reuse and the signed-in path. A mutation (committing instead of rolling back on an unconsumable invite) fails one of them.
- **ClickUp Copy can also complete the destination tasks.** `POST .../clickup/copy` and `POST .../clickup/bulk-copy` take an optional `complete_tasks` (default false: copying only comments is unchanged). When true, after a comment is posted the destination list's statuses are read (they differ per list) and the task is set to its complete status (`complete`/`completed`, else the list's `closed` class, else `done`). Each result carries `completed: {ok, message}` (absent when not asked for or when the comment failed); a list with no complete status says so and the comment still stands. Completing is only attempted after the comment succeeded. Costs two more ClickUp calls a destination, so the inline budget now fits about 10 facilities with the pointer note and completing (about 16 without completing); bigger copies run as background jobs as before. The activity log records `complete_requested` and `tasks_completed`.
- 5 new real-DB tests (off by default, completes when asked, no complete status, a refused comment is never completed, the dialog's option) and 2 call-budget unit tests.

## [1.9.94] - 2026-10-07

ClickUp Copy, phase 3 and 4: the client's bulk copy, and a rate limit with background jobs; the `Main tracker task` footer on copied comments; and dedup support for QuikStor Cloud's second header variant. Needs migrations `20261007140000` and `20261007150000`.

### Added
- **Bulk copy** (`api::clickup_copy::bulk`): one comment copied from a task in a source facility's list (the parent by default) to the counterpart task in any of the client's other facilities. `GET /clients/{id}/clickup/bulk-tasks` (the source's Set Up/Migration tasks, the facilities that can be destinations, and those with no list), `GET .../bulk-pairs` (each destination's suggested counterpart for the chosen task, with its tasks to choose by hand), `GET .../bulk-comment` (the prefill) and `POST .../bulk-copy`. A destination whose list cannot be read is reported on its own and does not fail the rest.
- **Per-user rate limit** (`clickup::rate_limit`): every ClickUp call a copy makes first takes a slot from a sliding 60-second window of 80 calls, under ClickUp's ~100/minute per token, shared by everything that user has running. Over the limit, calls wait their turn instead of failing rows with 429s.
- **Background jobs.** A bulk copy whose ClickUp calls fit in 50 (about 16 facilities with the pointer comment) runs inside the request. A bigger one is recorded in `client_ops.clickup_copy_jobs` and run by a background task, paced by the rate limit, saving progress after each batch; `POST` answers 202 with the job id and `GET .../copy-jobs[/{id}]` reports progress and per-facility results. Jobs are visible only to whoever started them (RLS). A job that says "running" but has not advanced for 5 minutes (the server restarted) is reported as `interrupted` when read -- there is no startup sweep, which would need to read every user's rows.
- The facility dialog's copy and the bulk copy now share one executor (`clickup_copy::exec`), so both go through the rate limiter and post the pointer the same way. `clickup_copy.rs` (about 700 lines) became a module: `lists`, `pairs`, `comments`, `copy`, `exec`, `bulk`, `jobs`.
- Each destination facility's activity log records what was copied onto it, for bulk copies too (`facility_clickup_comments_copied`, metadata `bulk: true`).
- 7 rate-limiter unit tests (a paused clock) and 8 bulk DB tests, including a real 17-facility background job and the owner-only visibility of jobs. `tokio`'s `test-util` feature is enabled for tests only.

### Dedup: QuikStor Cloud's street-address header variant and `Leases.csv`
- Davidson Road Self Storage's pull is the same QuikStor Cloud export as Freeland's but names the address columns `AddressStreet1/2`, `AddressCity`, `AddressState`, `AddressPostalCode` (plus a `Gender` column). The original row requires `AddressLine`, so **no file was recognized** and dedup refused a manual selection. Migration `20261007150000` adds three registry rows and leaves the originals untouched: `QuikStor Cloud Alternate Tenants (street address headers)` (supporting, inserted first because it is a header superset), `QuikStor Cloud (street address headers)` (primary, same `derive_quikstor_cloud_tenant_fields` transform, `LegacyTenantId` -> `TenantId`) and `QuikStor Cloud Leases` (supporting; the tenant-to-unit link, recognized but not joined yet). The original row's guidance no longer says leases are unsupported. The registry is cached 4 hours: restart the API after applying.
- 2 ingest tests and the real-migration-chain `the_seeded_registry_classifies_real_export_headers` now cover Davidson's headers. Davidson's real `Tenants.csv` through the real pipeline: 291 records, 291 tenants, 0 flagged, 4 duplicate customer records, 2 typo variants, 5 related candidates.

## [1.9.93] - 2026-10-07

ClickUp Copy, phase 2a: pairing two facilities' lists and copying comments between them. No migration.

### Added
- **`clickup_copy`**, three endpoints on the *target* facility (`integrations.clickup`, the caller's own ClickUp token): `GET .../clickup/copy-pairs` (the source and target lists' Set Up and Migration tasks, paired; optional `source_facility_id`, defaulting to the company's parent, and a Corp/Fac `scope` filter), `GET .../clickup/copy-comments` (one row's source comment for the prefill, plus whether the target looks already copied or already has the pointer -- read per row so opening the dialog does not read every task's comments) and `POST .../clickup/copy` (posts the edited comments; rows succeed or fail independently; up to 30 per request until the background queue exists). Audited as `facility_clickup_comments_copied`.
- **Main-list pointer.** Whenever a comment is copied, a separate generic comment "Main task list for this client is {parent's list}" (the name linked) is posted on the target task -- once per task, and never on the parent's own tasks or when no parent is designated.
- ClickUp client: dropdown **custom fields** on tasks resolved to the chosen option (Onboarding Phase, Corp/Fac), and `task_comments` (paged, newest first). Task pairing (`clickup::copy_pairing`) scores name and parent name, only within the same phase, one-to-one; "Set Up" and "Setup" are one phase.
- The duplicate-check ClickUp endpoints accept a run's **row id** as well as its session id (the Onboarding Work tab lists runs by row id), so a check can be posted to ClickUp later, from there. The run is still resolved to its own session id internally, so a captured Dropbox share link is stored against the right run.
- No marker is stored on what is copied, so "already copied" and "pointer already there" match on **wording** (`clickup::copy_text`); every such place is tagged `MARKER-TODO` for when an "Auto-added by OO" marker is added.

### Changed (after the first live test, AffStor)
- **Every copied comment now ends with a link to the task it came from**: three line breaks, then `Main tracker task - {source task name}`, the name linking to the source task. Added server-side (`clickup::copy_text::comment_parts`) so the facility dialog, the client's bulk copy and the background jobs all do it. `POST .../clickup/copy` items and `POST .../clickup/bulk-copy` take an optional `source_task_id` (looked up in the source facility's list, so the link is ClickUp's own; a task outside that list is refused with `task_not_in_source_list` and nothing is posted). The "already copied?" check ignores the footer. The separate once-per-task "main task list" note is unchanged.
- 3 new real-DB tests (dialog footer and link, bulk footer and link, a source task outside the source list is refused) and 3 footer unit tests.

## [1.9.92] - 2026-10-07

ClickUp Copy, phase 1: the prerequisites. Needs migration `20261007130000`.

### Added
- A company's **ClickUp parent facility** (the source ClickUp Copy will copy comments from): `clients.companies.clickup_parent_facility_id` and `PUT /clients/{id}/clickup-parent` (`client_ops.perform`, idempotent, audited as `client_clickup_parent_changed`). The parent must belong to the company and have a ClickUp list linked. Every change, including the first designation, is appended to the append-only `clients.company_clickup_parent_history` (names are snapshots, so history survives a rename or delete); the company detail response carries the parent id and the history.
- A **"no ClickUp project" waiver**: `clients.companies.clickup_waived_at`/`clickup_waived_by`, `PUT`/`DELETE /clients/{id}/clickup-waiver`, and a `clickup_waived` flag on `POST /clients` recorded in the create transaction. Lets a deliberate "no ClickUp" read differently from "nobody linked it yet".
- 10 real-database tests for both (`clients_clickup_parent_db_tests`).

## [1.9.91] - 2026-10-07

Efficiency refactor chunk D4f: `clients_detail.rs` (942 lines) split. No behaviour change.

### Changed
- `api/clients_detail.rs` became `clients_detail/`: `company/` (`dto`, `queries`, `handler`), `facility`, `policy_dto`, `policy_queries` (the per-table policy reads), `policies` (the handler that assembles them) and `tests`. `get_company_detail`, `get_facility_detail` and `get_facility_policies` keep their paths. The overlapping row structs shared with `clients_companies` and `clients_resync` were left alone (merging them is a separate, riskier change).

## [1.9.90] - 2026-10-07

Efficiency refactor chunk D4e: `clients_search.rs` split, `search_clients` (384 lines) broken up. Same responses.

### Changed
- `api/clients_search.rs` became `clients_search/`: `dto` (shapes), `matching` (person-derived facilities, Merchant Account correlation rows, near-miss names), `lookup` (the local database half, one RLS transaction), `display` (the live Merchant Account detail fetches), `handler` (`search_clients`, now ~120 lines of orchestration), `tests`, `lookup_db_tests`.
- `facility_matches_for` took nine arguments (`#[allow(clippy::too_many_arguments)]`); it now takes a `FacilityHit`, the correlation and a `DisplayLookups` struct, and the allow is gone.
- A failed local lookup is logged in one place. Every lookup failure now logs the user id and the query (some used to omit the query); the messages and the caller-facing errors are unchanged.

### Added
- A real-database test for `lookup::load` (people found by name, the rest of a matched facility's contacts folded in, already-imported detection); the lookups had no test beyond the hermetic ones, which stop at the live Process Street call.

## [1.9.89] - 2026-10-07

Efficiency refactor chunk D4d: `clients_resync.rs` split and `apply_resync` broken into steps. No behaviour change.

### Changed
- `api/clients_resync.rs` became `clients_resync/`: `rows` (DB row shapes), `fetch` (database and Process Street reads), `compare` (comparison types, the short-lived preview cache, conflict classification), `preview`, `apply`, `write` and `tests`. Public paths (`preview_resync`, `apply_resync`, `ResyncPreviewCache`) are unchanged.
- `apply_resync` (a 300-line function) now validates, loads the comparison, opens the transaction and calls `write::write_all`, which runs four named steps -- `update_company`, `update_facilities`, `rebuild_person_index`, `refresh_merchant_accounts` -- each returning an `ApplyError` that names the step. One place logs the failure, rolls back and chooses the response (a missing PII key is still its own 503). The SQL, the order of the statements and the log messages are unchanged; the real-database apply test passes unchanged.

## [1.9.88] - 2026-10-07

Efficiency refactor chunk D4b: `clients_elavon.rs` (1,233 lines) split. No behaviour change.

### Changed
- `api/clients_elavon.rs` became `clients_elavon/`: `mod.rs` (docs, shared `PERMISSION` and the two 409 responses, re-exports), `dto.rs` (response shapes), `build.rs` (decrypting a stored Merchant Account row into financials, credentials and parties), `get.rs`, `link.rs`, `unlink.rs`, `resync.rs` (one handler each, 150-250 lines), `tests.rs`. Handler paths (`clients_elavon::get_facility_elavon` etc.) are unchanged, so routes and the permission-gate tests are untouched.

## [1.9.87] - 2026-10-07

"Implementation Completed" for a client company. Pairs with `unitprep-ui` 1.6.63. **Needs migration `20261007120000`** (the code reads and writes `clients.companies.implementation_completed_at`).

### Added
- `clients.companies.implementation_completed_at` (nullable timestamp, a soft flag like `archived_at`).
- `PUT /clients/{company_id}/implementation-completed` marks the implementation completed and `DELETE` reopens it; both need `client_ops.perform`, both are idempotent (re-marking keeps the original time; only an unknown id is a 404), both write an audit event (`client_implementation_completed` / `client_implementation_reopened`).
- `implementation_completed_at` on the Clients directory list and on the company detail response, so the Clients page can group completed companies and the company page can show the toggle.

## [1.9.86] - 2026-10-07

Efficiency refactor chunk D3b: `main()` split. Startup behaviour unchanged except one error path (below).

### Changed
- `main.rs` (486 lines, a 397-line `main()`) is now a 21-line entry point calling `startup::run()`. New `src/startup/`: `cli` (subcommand dispatch, `run_bootstrap`), `config` (session timeout, ceremony timeout, WebAuthn settings, host/port), `logging` (tracing subscriber and panic hook), `state` (builds `AppState`; the Dropbox / Process Street config fallbacks and the WebAuthn backend are their own functions), `tasks` (every background task, started from the finished state), `serve` (bind, serve, graceful shutdown).
- A failure inside `axum::serve` used to `unwrap()` (a panic, exit 101); it now logs `server stopped with an error` and exits 1.
- Verified by booting the real binary from a cleared environment against the test database: `/health` and `/health/db` answer, SIGTERM logs the graceful shutdown and exits 0, a second instance on the same port prints the "already in use" guidance and exits 1, `--help` prints the usage.

## [1.9.85] - 2026-10-06

Efficiency refactor chunk D3a: the 1,311-line route table is split. No behaviour change.

### Changed
- `api/router/routes.rs` became `routes/`: `mod.rs` (`build()`, 50 lines, merges the groups), `rate_limited.rs` (the two per-IP limiters, their cleanup task and the routes behind them), and per-domain `account`, `clickup`, `client_ops`, `clients`, `integrations`, `tools` modules. The tagger-check body limit moved next to its route. Proved identical by dumping the route manifest (136 method/path/access entries) before and after: same set. The permission-gate tests are unchanged and green.

## [1.9.84] - 2026-10-06

Process Street task roles and hidden tasks: the Elavon and Onboarding Summary views no longer depend on one hardcoded task name. Pairs with `unitprep-ui` 1.6.61. **Needs migration `20261006120000`** (the code queries `ps_task_status.hidden` and `integrations.ps_task_role_name`).

### Added
- `ps_task_status.hidden` mirrors Process Street's own `hidden` flag (a 2026-10 template change renamed the credentials step to "Document Credentials" and left the old "Add Credentials to QMS" task on new runs, hidden). Every Onboarding Summary read ignores hidden tasks, as the cap and as the next step. Existing rows read as visible until their next resync.
- `integrations.ps_task_role_name` (admin-editable, RLS: any signed-in user reads, admin writes) maps a role to its Process Street task names; seeded with `qms_credentials` = "Document Credentials", "Add Credentials to QMS". `clients::ps_task_roles` loads and matches them; `GET /integrations/process-street/task-roles` and `PUT .../task-roles/{role}` (both `integrations.manage`).
- Onboarding Summary `elavon_complete`: the credentials step being Completed is the sole definition of Complete, independent of earlier open steps.

### Changed
- `credentials_added_to_qms_from_tasks` takes the role's names instead of a hardcoded string; the five callers (Elavon link/resync, manual link, client resync, client create) load them.

## [1.9.83] - 2026-10-06

Efficiency refactor chunk C0: dedup performance baseline. Test code only.

### Added
- `unitprep-dedup` `test-support` feature exposing `synthetic::synthetic_facility` (moved out of `performance_tests.rs`; deterministic fake tenants), enabled for the root crate's tests only.
- `api/dedup_pipeline_performance_tests.rs`: a budget test for the stages after the report (export plan, report view, CSV, XLSX at 2,400 rows) and an ignored `print_baseline` benchmark; `dedup` gained its own `print_baseline` (800 / 2,400 / 5,000 rows, best of three).

## [1.9.82] - 2026-10-06

Efficiency refactor chunk D4a: large inline test blocks moved out. No behaviour change.

### Changed
- Moved the inline test modules of `api/clients_resync.rs` (541 lines), `api/clients_search.rs` (519) and `clients/repository.rs` (758, the live-DB integration tests) into `clients_resync_tests.rs`, `clients_search_tests.rs` and `repository_db_tests.rs`, wired with `#[cfg(test)] #[path = ...]` like the existing `*_tests.rs` files. Test counts unchanged. Production files are now 1,076 / 778 / 813 lines.

## [1.9.81] - 2026-10-06

Efficiency refactor chunk D1: copy-pasted handler helpers consolidated. No behaviour change.

### Changed
- Deleted 12 byte-identical `request_context(headers)` copies (now `api::user_agent_from`), 7 `process_street_not_configured` and 3 `encryption_not_configured` copies (now shared in `api/mod.rs`), and 6 `bad_request` redefinitions (now `api::bad_request`; `clients_manual_link` passes its `invalid_request` code explicitly). The search/preview handlers keep their warn log at the call site. `clients_facility_policies_edit::bad_request` stays as a one-line wrapper that fixes the `invalid_request` code. About 200 lines removed.

## [1.9.80] - 2026-10-05

First ClickUp automation: post a finished duplicate check to the facility's ClickUp task. Pairs with `unitprep-ui` 1.6.59. Needs migrations `20261005130000` and `20261005140000`.

### Added
- **Duplicate check -> ClickUp task** (`api::clickup_duplicate_check`). `GET /clients/{company}/facilities/{facility}/clickup/duplicate-check-tasks?session_id=` lists the tasks in the facility's linked ClickUp list that look like this check's (1st or 2nd, decided from the run's position among the facility's dedup runs); `POST .../clickup/duplicate-check-results` comments "Duplicate check results are here" (*here* links the saved Dropbox file), adds the user as an assignee and sets the task to the list's complete status. Third and later checks only add a comment. A check that was only downloaded comments without a link. The task is re-read from ClickUp and refused unless it is in the facility's linked list; nothing is written if the list has no complete status; each of the three writes reports its own outcome; audited as `facility_clickup_duplicate_check_posted`.
- **Task names as data**: `integrations.clickup_task_steps` (migration `20261005130000`) holds the name phrases for each step; `clickup::task_matching` ranks tasks by word overlap, ignoring verbs, numbering, emoji and plurals, and penalizes a task naming a different ordinal.
- **Share link captured at save time** (migration `20261005140000`, `tool_runs.output_dropbox_link`): saving a dedup export to Dropbox creates the file's share link in the background and stores it, so the ClickUp update does not ask Dropbox again. `DropboxClient::shared_link`; falls back to the file's plain web path when Dropbox will not make a link.
- **Prefetch** (`api::clickup_prefetch`): `POST /integrations/clickup/prefetch` and `POST .../clickup/prefetch-tasks` answer 202 and warm the hierarchy / a facility's task list in the background (read-only, fire and forget).

### Changed
- **ClickUp calls are much faster**: independent reads and writes run together (the duplicate-check update went from ~15 s to ~1.7 s on the dev database), a list's tasks are fetched in parallel pages and cached 5 minutes, concurrent cold hierarchy loads share one load, a list already in the loaded hierarchy is confirmed without a ClickUp call, and the credentials read is one query. Saving links writes its audit rows together.
- A ClickUp client `send_json` helper for writes that are never retried.

## [1.9.79] - 2026-10-05

Efficiency refactor chunk E6: stale files and documentation. No code changed.

### Changed
- **`SCHEMA.sql` regenerated** (it was 15 migrations behind: missing `auth.user_permissions`, the ClickUp credential and settings tables, the facility ClickUp-link columns, tool-run additions, and every index change since). A new `scripts/regenerate_schema_sql.sh` rebuilds it from the local test database after `scripts/bootstrap_test_db.sh` has applied every migration (so it always reflects exactly what the migrations produce, needs no credentials and never touches Neon) and strips pg_dump's random `\restrict` token so a regeneration only shows real schema changes in the diff. A standing rule in `CLAUDE.md` says to run it with any migration-adding change.
- **`RUNBOOK.md` documents the production migration scripts**: `scripts/prod_db_status.sh` (read-only: which migrations prod is missing) and `scripts/prod_db_sync.sh` (applies them after an explicit `apply prod` confirmation, re-applies the `app_service` grants, verifies). These had no references anywhere and looked abandoned; they are in fact the hand-run production tooling, and the order of operations for a release with a migration is now written down. `README.md` now links `RUNBOOK.md`, `docs/DOCKER.md`, `AUTHENTICATION.md`, `THREAT_MODEL.md` and `SCHEMA.sql`, which had no inbound links.

### Removed
- **`README.sample.md`** (193-line drafted README redesign, never adopted, referenced by nothing). Archived verbatim in the vault (`work/archive/2026/UnitPrep/README Sample Draft (Unadopted).md`) and still in git history (`git show 9b19eab:README.sample.md`).
- Untracked, gitignored leftovers on disk (not part of the commit): two empty directories (`.sqlx/`, `examples/` -- the crate has no `query!` macros), two superseded environment backups (`.env.local.bak`, `.env.local.bak2`; every key they held also exists in the current `.env.local`, compared by name only), and two incomplete security-scan output directories (`CLAUDE-SECURITY-*`; zipped to `~/Desktop/stale-unitprep-artifacts-2026-10-05.zip` first).

### Kept deliberately
- `.env.local.pre-prod-app-pw` (until the production `app_service` password change is confirmed), `REFACTOR.md` (gitignored scratch), `dev-tools/` (the manual WebAuthn harness; not verified against the TOTP flow), `/health` and `/health/db` (infrastructure endpoints with no UI caller by design).

## [1.9.78] - 2026-10-05

Efficiency refactor chunk E3: four redundant indexes dropped. One migration; no code change.

### Changed
- **Migration `20261005120000` drops four single-column indexes that a UNIQUE constraint already covers**: `facility_merchant_account_parties (facility_id)`, `policy_coverage_tiers (facility_policies_id)`, `policy_delinquency_steps (facility_policies_id)` and `ps_task_status (facility_id)`. Each was on the leading column of a composite UNIQUE index the same table has, and a btree on `(a, b, ...)` serves every lookup on `(a)` alone, so the twin added nothing to reads while costing space and a write on every insert/update/delete. Verified against the migrated schema (`pg_indexes`), and the query planner was confirmed to use the composite index for single-column lookups after the drop. Foreign keys are unaffected. Reversible (the down migration recreates all four); applied, reversed and re-applied cleanly on the test database.

### Decided (left alone)
- **`clients.staff_identity_alias` is kept.** Its only reader (`staff_resolution`, never built) was removed in 1.9.74, so nothing uses the table -- but dropping a table is destructive and may hold data on the dev database, and it was created for a still-possible feature (Implementation Manager / Sales Rep assignment). Dropping it is a one-line migration whenever you decide that feature is not coming.
- **The columns with no Rust references** (`auth.user_roles.granted_at`, `auth.users.deletion_reason`, `auth.webauthn_credentials.transports`, `client_ops.tag_pattern.requires_rewrite`, `clients.policy_delinquency_steps.notice_channel`) are kept: several are constrained or granted by name and look reserved for features, and none costs anything meaningful.

## [1.9.77] - 2026-10-05

Efficiency refactor chunk E5: shared dependency versions across the workspace. A pure manifest change -- no code changed and nothing resolves differently.

### Changed
- **`[workspace.dependencies]` and `[workspace.package]`.** Thirteen dependencies were declared in two or more of the seven manifests (`serde`, `anyhow`, `tracing`, `csv`, `parking_lot`, `uuid`, `tokio`, `sqlx`, `zip`, `quick-xml`, `calamine`, `ts-rs`, `reqwest`), with feature sets that had already drifted (for example `serde` with and without `rc`, `tokio` with three different feature lists). Each is now declared once in the root manifest and referenced as `{ workspace = true }`; a crate that needs more features adds them (`{ workspace = true, features = ["rc"] }`). Every crate takes `edition` from `[workspace.package]`.
- **Verified identical:** `Cargo.lock` is byte-for-byte unchanged, the set of duplicate crate versions is unchanged, and the fully resolved feature graph (`cargo tree -e features`, 1,455 lines) is identical before and after.
- `chrono` is deliberately left out: the root crate uses its default features plus `clock`/`serde`, while `unitprep-core` uses `default-features = false`, and a workspace dependency cannot express both. Explained in a comment next to the workspace table.
- No unused dependencies were found (every crate's dependencies were checked against its source). The `reqwest` entries that appear in both `[dependencies]` and `[dev-dependencies]` of the root crate are intentional: the dev entry adds `multipart` for the HTTP integration tests.

## [1.9.76] - 2026-10-05

Efficiency refactor chunk E4: a real bug fixed -- Dropbox folder listings were silently truncated to the first page.

### Fixed
- **`DropboxClient::list_folder` now follows Dropbox's pagination** (`has_more` / `files/list_folder/continue`). It returned only the first page and ignored `has_more` (the field was `#[allow(dead_code)]`, with a comment saying it was "not needed" while the QMS Onboarding folder held 282 entries). A folder past one page (~2,000 entries) would have shown fewer files than it holds in the folder picker and the Dedup folder scan, with no error anywhere. A cursor that never ends is guarded (more than 100 pages is an error, not a silent truncation).

### Added
- **A test-only endpoint seam for the Dropbox client**, mirroring the ClickUp client's: every Dropbox URL now goes through an `Endpoints` value, and the override constructor exists only in test builds (`#[cfg(test)]`), so nothing in a release build -- no environment variable, setting or request -- can redirect the refresh token and app secret to another host.
- **Hermetic tests against a mock Dropbox** (axum on loopback): pagination across three pages (order, the continue requests carrying the right cursors, the namespace header on every page); a single page makes exactly one request; **a 401 refreshes the access token and retries once with the fresh one** (this path, added in 1.9.63, had no hermetic test because the OAuth URL was hard-coded); and a second 401 is returned as an error after exactly one refresh-and-retry, not retried forever.

## [1.9.75] - 2026-10-05

Efficiency refactor chunk E2: the superseded detect-vendor endpoints are removed, and an unrecognized Dedup file now says what it most resembles and what it is missing.

### Removed
- **`POST /dedup/detect-vendor` and `POST /dedup/detect-vendor-dropbox`** (handlers, response/request types, route entries, one test). They answered a single question for a single file ("which vendor is this?") and required uploading the whole file (or downloading it from Dropbox) to do it. They were the pre-Run-Check "confirm the vendor" gate, built 2026-08-20; since 2026-10-01 the folder-scan flow (`/dedup/classify-files`, `/dedup/classify-dropbox-folder`, `/dedup/file-requirements`) does the same for many files from just their header rows, and the UI stopped calling them. `/dedup/check` and `/dedup/import-dropbox` re-detect the vendor themselves, so nothing depended on them. The dead single-file multipart reader went with them.

### Added
- **"Looks like X, but is missing ..." for unrecognized files.** The classify responses (`POST /dedup/classify-files`, `/dedup/classify-dropbox-folder`) gain two fields on each file: `closest_vendor` (the registered format the file most resembles) and `missing_headers` (which of that format's required headers it lacks, in the format's own spelling); both empty/`null` when nothing resembles it and always for recognized or unreadable files. Resemblance is deliberately strict -- at least two of the format's required headers present and at least half of them -- because a wrong "looks like X" is worse than saying nothing. Ranked by headers matched, then fewest missing, then registry order. The change is additive (new fields only), so an older UI keeps working unchanged. Four tests cover it (names the format and the missing headers; no suggestion for a coincidental overlap; closest of several, ties to the earlier-registered; none for recognized/unreadable files); writing them caught a bug in my own tie-break (`max_by_key` keeps the last maximum, so registry order must be part of the key).

## [1.9.74] - 2026-10-05

Efficiency refactor chunk E1: dead code removed, and the compiler's dead-code detection turned back on for the `clients` module. Net -554 lines.

### Removed
- **`clients::ingest`** (280 lines) and **`clients::staff_resolution`** (236 lines): both were built as "Phase 1" modules that nothing ever called (their own headers said "no caller yet"); the only references were doc comments and their own tests. `ingest_facility` was superseded by `clients::create`; `resolve_staff_identifier` was a never-built follow-up (the `clients.staff_identity_alias` table it would have read remains, and is now a decision in plan chunk E3).
- **`ProcessStreetClient::list_workflows`** and its public re-export: nothing lists workflow *templates* (every workflow id is a known constant). The `Workflow` type stays, test-only, as the item type the pagination tests use.
- A stray unused test stub (`_unused`) in `api::clickup_lookup`.

### Changed
- **Seven stale module-level `#![allow(dead_code)]` removed** (`known_workflows`, `intake_mapping`, `merchant_account_mapping`, `people`, `encryption`, `fields`, `repository`). Their own headers said "remove once a real caller exists", and every module now has real callers; the blanket allows were hiding the genuinely dead items above. With the allows gone the compiler found exactly three more: `repository::ingest_intake_run` (now `#[cfg(test)]`, since only its live-DB test calls it) and the Contract Order pieces (`map_contract_order_fields`, `MappedContractOrder`, `repository::ingest_contract_order_run`), which are **deliberately on hold** -- kept, with a narrow item-level `#[allow(dead_code)]` and a comment saying why, instead of a module-wide one.
- Doc comments that pointed at the deleted modules were updated.

### Not changed (left deliberately)
- Items that are `pub` in library crates but only called from tests (`dedup::ingest::records_from_csv_document`, `docx_surgeon::edit_docx`, `FlatDocument::run_containing`, `clickup::hierarchy::invalidate`): the compiler does not flag them and narrowing visibility is cosmetic.
- `src/ai/` (the AI-integration placeholder), by instruction.

## [1.9.73] - 2026-10-05

Efficiency refactor chunk B3: the Process Street background sync no longer holds a database transaction open for minutes, fetches runs concurrently, and writes in batches. The largest single change of the refactor.

### Changed
- **No transaction is open while Process Street is called.** `run_all_workflows_with_progress` opened ONE transaction per workflow and, inside it, fetched each changed run's form fields from Process Street one at a time, then deleted and re-inserted that run's `ps_person_index` rows one statement per person, then upserted `ps_sync_state` -- so a workflow with thousands of changed runs held a pooled connection idle in a transaction for minutes and issued tens of thousands of single-row statements. Each workflow now runs in three phases (`sync_workflow_runs`): a short read transaction for the recorded `updatedDate`s; for each batch of 25 runs that need a refresh, fetch their fields **concurrently** (bounded to 6 in flight, `join_all_bounded`) with nothing held; then write that batch in its own short transaction (`apply_fetched_runs`) and commit it before the next batch's fetch starts.
- **Writes are batched.** Per batch: one `DELETE ... ps_run_id = ANY(...)`, one multi-row `INSERT ... FROM UNNEST(...)` of the fresh people, one multi-row upsert of `ps_sync_state`. (Intake runs still refresh their matching company/facility per run.) **Measured** through a 20 ms round-trip database proxy, 100 changed runs with two people each: write phase **8,776 ms -> 958 ms (9.2x)**, against a replay of the old per-row statement pattern (`db::tests`-style benchmark `sync_db_batching_benchmark`, `#[ignore]`d). Not measured: the network phase, which depends on Process Street's latency; it now runs six fetches at a time instead of one.
- **The three workflows' run lists are fetched together** instead of one after another.
- **Failure semantics, deliberately:** a failure partway (Process Street erroring after its retries, or a database error) still stops the sync and marks it `Failed` -- unchanged. But commits are now per batch, not per workflow, so everything already committed stays committed and the next delta check skips those runs (each run's `ps_sync_state` row is written in the same transaction as its person-index rows, so a run is never half-recorded). Previously a failure rolled back the entire workflow and the next attempt redid all of it. `GET` progress counts a skipped run as processed immediately and a refreshed run when its batch commits.
- A run id repeated in the list (Process Street's pagination can do this if the list changes mid-walk) is refreshed once; two rows with one key in a single batched upsert would be an error.

### Added
- Tests of the pipeline with a fake fetch (so no Process Street is involved): the pure delta decision (new / moved / unchanged / forced / duplicate ids) and two real-database tests (`sync_db_*`, `#[ignore]`d, local `test-db`): only changed runs are fetched and written, in batches, with stale person rows replaced, an unchanged run's rows untouched, and `business_dba` recorded; and a failed fetch stops the sync while keeping the already-committed batch. The pre-existing live-API tests keep working through a test-only `sync_one_run` wrapper that writes inside the caller's transaction so they can still roll back.
- The old code had no hermetic test at all (its only tests needed a live Process Street API key), so there is no before/after test pair for this chunk; the new tests assert the expected end state directly.

## [1.9.72] - 2026-10-05

Efficiency refactor chunk B4: independent work runs together, and every fan-out to an upstream API is bounded.

### Changed
- **Server boot reads run concurrently.** The five independent startup reads (the Dropbox and Process Street configs, which also decrypt, and the three vendor-format snapshots) were issued one after another; they are now one `tokio::join!`. **Measured** booting the real binary through a 20 ms round-trip database proxy: **0.63-0.72 s -> 0.32-0.35 s** to "listening" (about 2x; the gap grows with database latency and on a cold Neon compute).
- **Process Street run listings fetch their three statuses together.** `list_or_search_workflow_runs` fetched Active, then Completed, then Archived; it is now one `try_join!`, so the interactive search path pays one round trip's latency instead of three. (The number of calls against Process Street's hourly limit is unchanged.)
- **The two live Process Street searches in `GET /clients/search` run together** (facility-name and Merchant-Account-name searches were awaited one after the other although neither depends on the other).
- **Dropbox folder imports download a few files at a time** (unit-group `import_from_dropbox` and the Dedup Dropbox check) instead of one by one, keeping results in folder/selection order.
- **Every data-driven fan-out to an upstream API is bounded.** The `join_all` batches in client search, import preview, Re-sync (fields and Merchant Account runs) and create-from-Process-Street fired one request per cited run all at once -- risking Process Street's ~2,500 requests/hour limit, which retries (1.9.63) would then make worse. They now go through `integrations::http::join_all_bounded`: at most 6 in flight, results in input order (a drop-in for `join_all`).

### Added
- `integrations::http::join_all_bounded` and `MAX_CONCURRENT_UPSTREAM_CALLS`, with a test that concurrency never exceeds the limit, actually exceeds 1, and results stay in input order. (It is a plain function that collects its futures up front, like `join_all`: an `async fn` version held the lazy iterator across the await and tripped the compiler's higher-ranked-lifetime check inside axum handlers.)

### Not changed
- `GET /clients/filter-options`' three queries and the per-search read of every Process Street run title (`merchant_account_run_titles` / `all_intake_run_titles`) -- low value here, tracked in the plan.

## [1.9.71] - 2026-10-05

Efficiency refactor chunk B2: client Re-sync no longer holds a database transaction open while it calls Process Street, and rebuilds the person index in two statements instead of dozens.

### Changed
- **No transaction is open during the Process Street calls.** `preview_resync`, and `apply_resync` when there is no cached preview, built their comparison (`load_comparisons`) inside a database transaction: the transaction read the rows, then sat open -- holding one of the pool's 20 connections idle -- while Process Street was called for every cited run's fields and every linked Merchant Account run's fields and tasks (seconds for a company with many facilities), then (for apply) did its writes. `load_comparisons` is now three phases in this order: a short read transaction that commits, the network fetch with nothing held, then pure assembly. `apply_resync` now builds the comparison (on a cache miss) *before* opening its write transaction. The rows an uncached apply acts on are therefore a few seconds old by the time the writes run -- the same staleness window the preview-cache path has always had, for up to five minutes.
- **The person-index rebuild is two statements, not one per run plus one per person.** Apply refreshed `clients.ps_person_index` with a `DELETE` per run and an `INSERT` per person, each its own round trip inside the transaction (a company with ten runs and a few people each paid sixty-odd round trips before committing). It is now one `DELETE ... ps_run_id = ANY(...)` and one multi-row `INSERT ... SELECT FROM UNNEST(...)`, however many runs and people there are.

### Added
- Real-database test (`resync_db_apply_replaces_a_runs_person_index_and_leaves_other_runs_alone`, `#[ignore]`d, local `test-db`): seeds a preview into the cache (so no Process Street call is needed), applies it, and checks the run's index rows are exactly the fresh people (the stale one gone, each carrying the fallback run name) and another run's rows are untouched. It was written against the old per-row loop first and passes unchanged against the batched version.

## [1.9.70] - 2026-10-05

Efficiency refactor chunk B1a: database pool tuning. Removes a hidden network round trip from every pooled database use.

### Changed
- **The pool no longer pings every connection on every acquire.** sqlx's default (`test_before_acquire(true)`) sends a full extra round trip before the caller's own first statement -- on every RLS transaction, every session lookup and every pooled audit write in the app. The pool (`db::pool_options`) now pings only connections that have been idle 30 s or more (a dead one is discarded and replaced, not handed to a request) and trusts one that was in use moments ago. The residual risk is a connection that dies inside that window, which fails the single statement that finds it.
- **`acquire_timeout` is 10 s** (sqlx default 30 s), so a starved request fails in a bounded, visible way instead of hanging for half a minute, while still leaving room for a cold start of a suspended Neon compute.

### Measured
- Through a latency-injecting proxy in front of the local test-db (new `dev-tools/latency_proxy.py`, 20 ms round trip, plausible for Neon): one handler-style transaction (acquire, BEGIN + `set_config`, one query, COMMIT) **105.5 ms -> 84.2 ms**, i.e. exactly the one round trip saved (5 -> 4). The same saving applies to the session lookup (`resolve_session`) and every audit write, so a typical authenticated request saves two to three round trips. Benchmark: `db::tests::pool_ping_policy_latency_benchmark` (`#[ignore]`d; usage in its doc comment).

### Decided
- **The client detail endpoints keep their parallel transactions.** The plan's fallback of running the page's 4 / 10 queries sequentially on one transaction was rejected: the code's own 2026-09-03 note records that the parallel form was a deliberate fix for a real, visible load delay against remote Neon, and sequential on one connection would cost ~13 round trips instead of ~4 on the wall clock. A single bundled query (one SQL function returning one JSON document) is the only change that would cut connection use without costing latency; it is tracked as optional plan chunk B1b.

### Added
- `dev-tools/latency_proxy.py`: a tiny TCP proxy that adds artificial latency in front of the local test-db, for benchmarking round-trip behaviour against something closer to a remote database than loopback.
- Tests: the pool's configuration is asserted (max 20, 10 s acquire timeout, unconditional ping off); an `#[ignore]`d real-DB test exercises both branches of the idle-gated ping.

## [1.9.69] - 2026-10-05

Efficiency refactor chunk A6: recording a Dedup run no longer encrypts and serializes on an async worker, and no longer does it while holding a database connection. This finishes Phase A of the plan.

### Changed
- **`create_dedup_run` seals off the async workers.** It encrypted the uploaded source file and serialized + encrypted every tenant record (the whole tenant list) directly on the async worker, *after* opening the RLS transaction -- so a pooled connection was held through the CPU work. It now does the sealing on the blocking pool first and opens the transaction only when the ciphertext is ready. If the sealing task itself panics the run is still recorded, without a stored copy or rematch records (the same degradation as a missing encryption key).
- `ToolRunCreate.records` is now an owned `Vec<TenantRecord>` (the blocking pool needs `'static` data); both callers move their records in, so nothing is cloned.
- The span-preserving `spawn_blocking_in_span` primitive moved to a new crate-root `blocking` module so non-HTTP code (`client_ops`) can use it without importing from `api`; `api::blocking` re-exports it and keeps the handler-facing wrappers.

### Not needed
- **Session persistence serialization** (`DurableSessionStore::persist`'s `bincode::serialize`) was in the plan for this chunk, but after 1.9.66-1.9.68 every heavy `save()` (session creation and the unidentified-mode re-check) already runs inside a blocking-pool closure; the remaining callers are the tiny WebAuthn ceremony saves. Write ordering/coalescing of those upserts stays with plan chunk B5.

### Added
- Real-database test (`tool_run_create_db_*`, `#[ignore]`d, local `test-db`): the stored source is ciphertext (never the plaintext, which can carry card numbers) that opens back to the original under the run's session id, the kept records round-trip, and with no key configured the run is still recorded with no stored copy.

## [1.9.68] - 2026-10-05

Efficiency refactor chunk A4b: the unit-group (Group Prep) compute that runs under a session lock no longer runs on the async worker threads.

### Changed
- **Discovery, validation, analysis and export compute run on tokio's blocking pool.** Many Group Prep handlers do their real work -- re-deriving the whole discovery, validating every document, mapping a vendor format, building the analysis batch, rendering and zipping the export CSVs -- inside the closure they hand the session store, i.e. under the session lock, on an async worker. Now off the workers: `POST /discover`, `/unit-file/select`, `/unit-file/resolve-format`, `/unit-file/upload`, `/group-file/select`, `/group-file/confirm`, `/group-file/upload`, `/validate`, `/analyze` (both the document preparation under the read lock and the batch build + analysis itself) and the unit-group `/export` (CSV generation and ZIP build). The lock semantics are unchanged: it is taken and released inside the single blocking call.
- New `with_owned_session_mut_blocking` / `with_owned_session_blocking` helpers (next to `run_blocking` in `api::blocking`) carry the session-store closure onto the blocking pool, with the request's tracing span and the same panic-to-500 mapping. Handlers share their parsed request into the closure through an `Arc`, so the closure bodies are textually unchanged.
- A panic inside any of these now becomes the standard `internal_error` response (logged with the operation name) via the helper, as before via `CatchPanicLayer`.

### Not changed
- The light "flip a flag" session mutations (`/correct`, `/correct-group`, `/exclude-group(s)`, `/exempt`, `/acknowledge-group-warnings`, `/cancel`) stay inline: they do no meaningful work under the lock.

## [1.9.67] - 2026-10-05

Efficiency refactor chunk A5: the Dedup report view and export generation no longer run on the async worker threads.

### Changed
- **Dedup report-view building, export generation and re-checks run on tokio's blocking pool.** `build_report_view` assembles the whole export plan (grouping, typo-variant and related-tenant sections over every tenant) and `generate_export` writes an XLSX/CSV/ZIP in memory; both were called straight from `async fn` handlers. Now off the workers: `POST /dedup/check` and the Dropbox-import check (the view step; the report itself already moved in 1.9.66), `POST /dedup/report`, `POST /dedup/export`, `POST /dedup/export-to-dropbox`, the live-session "unidentified tenants" re-check (`set_unidentified_mode`, which re-runs the whole report), and the tool-run re-check including regenerating its stored output file.
- New `api::dedup_blocking` (`report_view`, `export`) wraps the two pure functions for every Dedup handler. They take the report and records **by value and hand them back** (the blocking closure must own what it touches, and every caller still needs the data for the tool-run record, audit event or log line), so no extra clone is introduced -- except in the tool-run regeneration path, which already held borrowed data and now clones it once (to be removed by the clone-reduction work, plan chunk C2).
- `ExportFormat` is now `Copy` (it is moved into the blocking closure).
- A generation failure in `POST /dedup/export` now returns its error response directly; previously the handler also logged "Dedup export generated" for a failed export.

## [1.9.66] - 2026-10-05

Efficiency refactor chunk A4: spreadsheet parsing no longer runs on the async worker threads.

### Changed
- **File parsing and session creation now run on tokio's blocking pool.** Parsing an uploaded xlsx/csv is CPU-bound, and called straight from an `async fn` handler it occupied one of the runtime's few worker threads for its whole duration -- stalling every unrelated request scheduled on that worker, including the auth lookup each authenticated request starts with. Moved off the workers: the unit-group upload and Dropbox import (`SessionService::create_session`), the manual group-file and unit-file uploads, the dedup check and Dropbox-import check (`DedupSessionService::create_session`, which also builds the whole report, so this also covers part of chunk A5), and each file's parse in the Dedup folder scan (up to `DROPBOX_SCAN_CONCURRENCY` at once; a panic parsing one file now reports just that file as unreadable instead of failing the scan).
- **New `api::blocking` helper** (`run_blocking`, `spawn_blocking_in_span`) used by every site above. It carries the request's tracing span onto the blocking thread -- a plain `spawn_blocking` closure runs with no current span, so every log line inside the parsers ("Skipping file", "Creating session") would have silently lost its `request_id` -- and turns a panic in the work into the project's standard `internal_error` 500 (logged with the operation name), exactly like a panic in the handler body already did.

### Not changed (deliberately)
- The two detect-vendor handlers (`POST /dedup/detect-vendor*`) still parse inline: they are dead code scheduled for removal (plan chunk E2).
- Unit-group `validate` / `analyze` / discovery work that runs under a session write lock is a separate piece (plan chunk A4b), as is the dedup report/export generation (A5).

### Added
- Tests for the helper: the runtime keeps running other tasks while blocking work runs (single-thread runtime, so it would fail if the work ran inline), a panic becomes the standard 500, and the caller's tracing span is entered on the blocking thread.

## [1.9.65] - 2026-10-05

Efficiency refactor chunk A7: gzip response compression.

### Changed
- **Large JSON and CSV responses are now gzip-compressed** for clients that send `Accept-Encoding: gzip` (every browser does). The dedup report view, client/facility detail, search results, audit-log pages and CSV exports are highly repetitive text that gzip typically shrinks 80-90%, which is most of the transfer time for a user on an ordinary connection. Implemented as a `tower-http` `CompressionLayer` (`api::router::compression`), applied outside the panic/rejection layers and inside the request-id/trace layers.
- **Already-compressed formats are deliberately excluded**: ZIP, the OOXML formats (XLSX dedup exports, DOCX tagger output), and PDF. `tower-http`'s default predicate also skips bodies under 32 bytes, images, gRPC and server-sent events, and a response that already carries a `Content-Encoding` is left alone, so a reverse proxy that compresses in front of this service will not double-compress.

### Dependencies
- `tower-http` gains the `compression-gzip` feature (gzip only; brotli would add a second compression crate for a marginal gain on this traffic). New transitive crates: `async-compression`, `compression-codecs`, `compression-core`, `tokio-util` (`flate2` was already in the tree). Recorded in the vault's tech-stack note.

### Added
- Tests for the layer on a toy router (gzip for JSON/CSV with a real gzip stream well under the original size; no compression when the client does not ask; ZIP/XLSX/DOCX/PDF untouched; tiny bodies untouched; pre-encoded responses not double-compressed) and one against the **real** router proving the layer is actually wired in.

## [1.9.64] - 2026-10-05

Efficiency refactor chunk A3: indexes for the Process Street run-id lookups.

### Changed
- **Index the PS run-id columns that are searched with `= ANY($1)`** (migration `20261005110000`): `clients.facilities.ps_intake_run_id`, `clients.companies.ps_intake_run_id` and `clients.facility_merchant_accounts.ps_new_merchant_run_id`. Client search, import preview and create-from-Process-Street ask "which of these run ids are already imported?" on every call, and none of the three columns had an index, so each was a sequential scan that grows with every client onboarded. The indexes are partial (`WHERE ... IS NOT NULL`, so manually created rows are not indexed) and deliberately not unique (a sister-facility import may point two rows at one run).
- Measured on 200,000 synthetic facilities (rolled-back transaction on the local test-db), a 30-id lookup went from a parallel sequential scan at 26.96 ms to an index-only scan at 0.31 ms (about 88x). Invisible at today's row counts; this keeps those lookups flat as the data grows.

### Decided
- **`pg_trgm` for the `ILIKE '%q%'` person/facility search is deferred**, as migration `20260831140000` already decided: the btree indexes on `lower(full_name)` / `lower(email)` cannot serve a leading wildcard, but the tables are small. Revisit when `ps_person_index` passes roughly 50k rows or search latency becomes visible.

## [1.9.63] - 2026-10-05

Efficiency refactor chunk A2: one outbound-HTTP policy for Process Street, Dropbox and ClickUp. A hung or flaky upstream can no longer pin a request (or every Dropbox call) indefinitely.

### Changed
- **Timeouts everywhere.** The Process Street and Dropbox clients were built with `reqwest::Client::new()`, which has no timeout of any kind. Both now use the shared builder in the new `integrations::http` module: 5 s connect timeout, 30 s overall request timeout. Dropbox file download/upload get a 300 s per-request ceiling instead, since they move real payloads. ClickUp keeps its 15 s limit but now uses the same builder.
- **Transient failures are retried.** A connect error, a timeout, or an HTTP 429/500/502/503/504 is retried up to 3 times with 0.5 s / 1 s backoff plus jitter (Process Street and ClickUp reads; Dropbox reads and the idempotent `create_folder_v2`). A numeric `Retry-After` is honoured, but one longer than 10 s is not waited out: the response is returned so a user-facing request fails fast. Permanent errors (401, 404, ...) are never retried, and the overwrite upload is deliberately never retried.
- **Dropbox token refresh is bounded and self-healing.** The OAuth refresh ran while holding the token mutex with no timeout, so one hung refresh stalled every Dropbox call app-wide; it now has a 10 s timeout and at most one retry. A 401 from any Dropbox call now drops the cached token, fetches a fresh one and retries once, instead of surfacing an expired-token error to the user.
- **ClickUp shares one connection pool.** `clickup_client()` built a new `reqwest::Client` (new pool, new TLS handshake) on every handler call; the underlying client is now created once per process and cloned (a cheap handle).
- **Upstream error bodies are truncated in logs.** Process Street and Dropbox error responses (which can echo customer data) were logged in full at error level; log lines now carry at most 512 bytes. The full body still travels in the returned error.

### Added
- `integrations::http`: `client_builder`, `send_with_retry` / `RetryPolicy`, `truncate_for_log`, with loopback-server tests for success-after-retry, retries exhausted, permanent errors, `Retry-After` (short and over the cap), timeouts, refused connections, backoff bounds and multibyte-safe truncation. Process Street (`get_page`) and Dropbox (`send_authed`) each have a loopback test of the retry path.

## [1.9.62] - 2026-10-05

First step of the efficiency refactor (plan: vault `work/active/UnitPrep/Efficiency Refactor/`, chunk A1): fewer database writes and round trips on every authenticated request. No behaviour change except the idle timeout can fire up to 60 s earlier than before (never later).

### Changed
- **`auth.resolve_session` no longer writes to `auth.sessions` on every request.** It used to be one `UPDATE ... SET last_seen_at = now() ... RETURNING`, so every authenticated request took a row lock and wrote WAL, and the SPA's parallel requests serialized on that lock. Lookup and bump are now separate CTEs (migration `20261005100000`): validity (token, not revoked, absolute expiry, idle window, user active and not deleted) is evaluated on every request exactly as before, but `last_seen_at` is rewritten only when it is more than `LEAST(60 s, 6 s per idle minute)` old. Revocation and deactivation still take effect on the very next request. The stored value can lag real activity by up to the throttle interval, so a session can expire up to that much **early**, never late (effective idle window `[idle - throttle, idle]`). Same signature, same returned columns, same `app_service` grant; the down migration restores the unconditional bump.
- **`begin_rls_transaction` sets both identity GUCs in one statement** (`app.current_user_id` and `app.current_user_roles` in a single `SELECT set_config(...), set_config(...)`), saving one round trip at the top of every RLS-scoped handler (~195 call sites). Pre-handler overhead is now resolve + BEGIN + 1 `set_config` (+ COMMIT) instead of resolve + BEGIN + 2 `set_config` (+ COMMIT).

### Measured
- `pgbench`, 8 clients hammering one session on the local test-db (a worst case for row-lock contention; real gains over a network are smaller): `resolve_session` 10.8k -> 37.4k calls/s, average latency 0.74 -> 0.21 ms, and 86,650 session-row updates / 2,910 dead tuples -> 0 / 0.

### Added
- Real-database tests (`session_resolution_db_*`, `#[ignore]`d, local `test-db` only): a stale `last_seen_at` is bumped once then left alone inside the throttle interval (verified to FAIL against the old function); an idle session is rejected and never resurrected; revoked and deactivated sessions are rejected on the next request; the returned columns are unchanged; `begin_rls_transaction` sets both GUCs and they do not leak onto the pooled connection.

### Docs
- `AUTHENTICATION.md` and `THREAT_MODEL.md` now describe the throttled bump instead of "bumped on every request".

## [1.9.61] - 2026-10-05

Stops sister facilities being offered the wrong Merchant Account form, and lets a facility without its own form borrow Legal Owners from a sister's.

### Fixed
- **Elavon tab / search / import preview offered a sister's Merchant Account form.** Affordable Storage's only form is titled "Affordable Storage (Beau Ryan) Katy-Flewellen"; "Beau Ryan" is the owner and the tail of every sister's Intake title, so Copperfield (and others) were offered Katy-Flewellen's form. `correlate_by_title` now takes the full set of Intake titles and ignores any keyword that appears in more than one facility's title (it names a company or owner, not a facility). The run's real facility still matches through its own DBA.

### Added
- **Legal Owner fallback.** A facility with no Merchant Account owners of its own now takes its Legal Owner checkmarks (and "missing owner" chips) from a sister facility in the same company: the sister whose form lists the most named owners, then the most recently synced. A form with only a signer or blank owner slots does not count as having owners; another company's forms are never used. `GET .../people` returns `legal_owner_source` (the sister facility) so the UI can say so.
- Real-database tests for the fallback (`people_db_*`) and regression tests for the Beau Ryan case.

## [1.9.60] - 2026-10-02

Link facilities to their ClickUp onboarding lists.

### Added
- **Facility -> ClickUp list links** stored on `clients.facilities` (`clickup_list_id/name/url`, `clickup_folder_name`, `clickup_linked_by/at`; all-or-nothing CHECK). Only the list id is authoritative; the rest is a snapshot taken at link time. Many facilities may share a list; the API reports it rather than blocking.
- **Fuzzy matching** of a facility to ClickUp lists (`clickup::matching`): IDF-weighted token overlap (rare words such as a town outrank brand words), the facility's city folded into the query, a small legal-entity folder bonus, generic words dropped, typo tolerant. Non-facility lists (Post-/Pre-Onboarding, templates, sandboxes, "General / Multiple Sites", training, demo) are excluded. Confidence is High only with a strong score *and* clear daylight over the runner-up. `clickup::assignment` never suggests the same list for two facilities.
- **ClickUp URL parser** for "Link manually" (`clickup::url`): list URLs, the list-view URL from the address bar (resolved to its parent list), bare ids; refuses look-alike hosts, folders, tasks, docs.
- **Endpoints** (per-user `integrations.clickup`, caller's own token): `GET /integrations/clickup/lists`, `POST /integrations/clickup/resolve-url`, `GET /clients/{id}/clickup/suggestions`, `PUT /clients/{id}/clickup/links` (all-or-nothing; every list re-verified with ClickUp, name/URL taken from ClickUp), `DELETE /clients/{id}/facilities/{fid}/clickup-link`, `DELETE /clients/{id}/clickup/links`. Unlinking makes no ClickUp call. Audited as `facility_clickup_linked` / `facility_clickup_unlinked`.
- `integrations.clickup_settings` (onboarding space looked up **by name**, default "QMS Onboarding"), readable by any signed-in user, editable by admin/developer.
- Company and facility detail responses carry the link fields.
- Eight real-database tests against a mock ClickUp, plus unit tests for the matcher (using a slice of the real hierarchy), URL parser, assignment, and client.

### Fixed
- ClickUp answers HTTP 401 both for a bad token (`OAUTH_025`) and for "no such list / no access" (`OAUTH_027`); the client now tells them apart so a mistyped list URL can never mark a good token invalid.
- A facility write by a user with ClickUp but no client-ops role now returns a clear 403 instead of a silent no-op.

### Migrations
- `20261002210000_add_facility_clickup_links`.

## [1.9.59] - 2026-10-02

Per-user permission grants and the first ClickUp integration step (connect and verify a personal API token).

### Added
- **Direct per-user permission grants** (`auth.user_permissions`) alongside role-derived ones, merged into the resolved permission set in `auth.resolve_session` so every existing check works unchanged. Only permissions flagged `directly_grantable` can be granted this way (database trigger), RLS limits writers to `admin`/`department_manager` and forbids self-grants. Endpoints: `GET /auth/users/{id}/permissions`, `PUT|DELETE /auth/users/{id}/permissions/{key}` (idempotent, audited).
- **ClickUp connection, per user**: `integrations.user_clickup_credentials` (ChaCha20-Poly1305 ciphertext, AAD bound to the user, owner-only RLS even against admins). `GET /integrations/clickup/connection`, `PUT|DELETE /integrations/clickup/token`, `POST /integrations/clickup/test`, all behind the new per-user `integrations.clickup` permission. A token ClickUp rejects is never stored; an unreachable ClickUp does not mark a stored token invalid.
- **`users.view`** permission (list only) so department managers can see the Users page and grant personal-integration permissions without invite/disable/recover/export/role powers; `auth.user_exists(uuid)` for their existence checks (`auth.users` is not readable by them under RLS).
- New audit events: `permission_granted`, `permission_revoked`, `integration_connected`, `integration_disconnected`.
- Seven real-database `clickup_db_*` tests (local ephemeral test-db, as `app_service`, so RLS genuinely applies).

### Changed
- `auth.list_users_for_admin()` now also admits `department_manager`; `GET /auth/users` is gated on `users.view` (CSV export stays `users.manage`).

### Migrations
- `20261002180000_add_user_permission_grants`, `20261002190000_create_user_clickup_credentials`, `20261002200000_let_department_managers_grant_permissions`.

## [1.9.48] - 2026-09-29

Builds the CI/CD framework's Tier 1: GitHub Actions CI.

### Added
- **`.github/workflows/ci.yml`** — `fast-checks` (fmt, clippy, `cargo check`) on every push to `main`; `full-tests` (fast suite + DB-only `#[ignore]`'d tests against a genuinely ephemeral `postgres:18` GitHub Actions service container) only on a version-tag push. Credential absence (isolation control #1) enforced by omission — this workflow never references any Neon secret. Both jobs pass `actionlint` cleanly.
- **`scripts/run_ci_db_tests.sh`** — an explicit allowlist of the 17 `#[ignore]`'d tests that need only *a* real Postgres, never Process Street/Dropbox access, determined by reading every test's own `#[ignore = "..."]` reason string rather than guessing from names (one safe test is literally named `...dropbox...` despite needing no real Dropbox access at all). Verified locally against the real ephemeral test-db before wiring into CI — all 17 passed.

### Changed
- **`bootstrap_test_db.sh`'s host/port/credentials are now overridable via env vars** (defaulting to the existing Docker values) instead of hardcoded, so the same script works unchanged against GitHub Actions' service containers too.

## [1.9.47] - 2026-09-29

Closes the `TEST_DATABASE_URL` isolation-control gap and an RLS-bypass regression, both found via an external (Grok) review of the Docker setup.

### Added
- **`db::connect_test()`** — implements the CI/CD framework's isolation control #2/#3 for real: a distinct connection path for `#[ignore]`'d real-DB tests, reading `TEST_DATABASE_URL` only (never `DATABASE_URL`), hard-failing if unset or malformed, and aborting loudly if the resolved host looks like a Neon endpoint. 24 call sites across 12 files updated to use it; `main.rs`'s real application startup is unchanged.

### Fixed
- **`TEST_DATABASE_URL` connected as the `postgres` superuser**, which bypasses row-level security entirely — tests were passing without actually proving RLS held. Now connects as `app_service` (a real, local-only password set by `bootstrap_test_db.sh`), matching how the real application connects.
- **A real bug in `scripts/setup_app_service_role.sql`**: each schema's grants were bundled into the same `DO` block as an `ALTER DEFAULT PRIVILEGES FOR ROLE neondb_owner` statement that always fails on a non-Neon Postgres — and an uncaught exception anywhere in a `DO` block silently rolls back the *entire* block, not just the failing statement. `app_service` ended up with no access to `auth` (or `client_ops`, `integrations`, `clients`) at all. Fixed by wrapping just that one statement in its own nested exception handler per schema (5 occurrences); behavior against real Neon is unchanged (the exception never fires there).

## [1.9.46] - 2026-09-29

### Fixed
- **Passkey login/registration failed against the dev containers** (`"The clients relying party origin does not match our servers information"`) — same root cause as `v1.9.45`'s CORS fix, different mechanism: WebAuthn ceremonies validate the browser's origin cryptographically against a single `WEBAUTHN_RP_ORIGIN` value (no list support, unlike CORS), which still defaulted to `localhost:3000` while `ui-dev` runs on `3001`. Set explicitly in `api-dev`'s compose environment.

## [1.9.45] - 2026-09-29

Two fixes found live-testing Docker Phase 2/3 together for the first time.

### Fixed
- **CORS blocked `unitprep-ui`'s dev container from reaching the API** — `CORS_ALLOWED_ORIGINS` only defaulted to `localhost:3000`/`:5173`, but `ui-dev` runs on `3001` (remapped due to a port collision with a native `next dev`). Added both origins to `api-dev`'s compose environment. The frontend's "could not reach the API" error was misleading — the request was reaching the server the whole time; the browser was silently discarding the response on CORS grounds.

### Added
- **Docker-aware guidance in the "port already in use" startup error** — the existing `ss`/`lsof` advice only helps when the other instance is on the host; inside `api-dev`, a leftover process lives in the container's own network namespace, invisible to host-level tools. Added a line pointing at `docker compose up -d --force-recreate api-dev` instead.

## [1.9.44] - 2026-09-29

Docker Phase 2 of the CI/CD framework's containerization plan.

### Added
- **`Dockerfile.dev` + `docker-compose.yml`'s `api-dev` service** — a long-lived `unitprep-api` dev container, source bind-mounted (never rebuilt per code change), auto-running `cargo watch -x "test --workspace"` continuously. Four named volumes (`cargo-registry`, `cargo-git`, `cargo-rustup`, `api-target`) provide the actual caching, since cargo-chef's build-time dependency pre-baking would just get shadowed by volumes mounted at the same paths — deliberately skipped here, see `UnitPrep Docker Standards.md`'s Amendments section. Port `8080` forwarded for on-demand manual testing (`docker compose exec api-dev cargo run`, kept separate from the auto-loop so an unrelated edit never disrupts a manual testing session).
- **`scripts/bootstrap_test_db.sh`** — makes a freshly-recreated `test-db` (genuinely ephemeral by design) actually usable for `#[ignore]`d real-DB tests in one idempotent command, instead of a fragile manual multi-step sequence.

### Fixed (during verification, not shipped as bugs)
- `rustup`'s component store wasn't in a named volume, so `rustfmt`/`clippy` were silently re-downloading on every container recreation, not just image rebuilds.
- The first attempt at running an `#[ignore]`d test against a freshly-recreated `test-db` failed (`relation "auth.roles" does not exist`) — not a bug, just the ephemeral design working as intended; `bootstrap_test_db.sh` closes the gap.

## [1.9.43] - 2026-09-28

Docker Phase 1 of the CI/CD framework's containerization plan.

### Added
- **`docker-compose.yml`** — a single `postgres:18` service (`test-db`, gated behind a `test` compose profile) for local ephemeral test-DB isolation, matching the real Neon dev branch's Postgres version (18.6). No named volume for the data directory (tmpfs instead) so it cannot outlive the container. Bound to `127.0.0.1:5433`, not `5432` — this machine already has a native Postgres listening there. Verified empirically end to end: brings up healthy in ~3s, `psql` confirms PostgreSQL 18.6, tearing down leaves zero volumes behind.

### Fixed (during verification, not shipped as a bug)
- Mounting tmpfs at the traditional `/var/lib/postgresql/data` crashed the container on startup — the `postgres:18+` image switched to a `pg_ctlcluster`-style layout expecting the parent `/var/lib/postgresql` instead. Caught by actually running it, not assumed from the older convention.

## [1.9.42] - 2026-09-28

The two Tier-0 tooling gaps the CI/CD framework doc flagged as "next, not urgent" — `cargo-audit` and `gitleaks` — installed and wired into `preflight.sh`, plus the one real vulnerability the first `cargo audit` run against this codebase actually found.

### Added
- **`cargo audit` as preflight step 4/7** — blocks a push on a real RUSTSEC vulnerability, but not on advisory-grade `unmaintained`/`yanked` warnings (those print, don't fail the script).
- **`gitleaks` as preflight step 5/7**, diff-scoped to `merge-base(origin/main, HEAD)..HEAD` — the real secrets scanner the existing grep-based step was always meant to be backed up by, not replaced by; the grep step stays in place as a second layer.
- **`.gitleaks.toml`** — allowlists 6 confirmed false positives in `src/clients/testdata/highway20_*.json` (synthetic Process Street workflow/task IDs sitting next to a `"key"` field trip the `generic-api-key` entropy rule; verified against the sanitized fixture's actual content, scoped to just those two files rather than the whole testdata directory).

### Fixed
- **`RUSTSEC-2026-0285`** (`rustls` TLS 1.3 handshake messages incorrectly accepted across encryption level boundaries) — a real, medium-severity vulnerability. `rustls` is a transitive dependency (via `reqwest`/`sqlx`, not pinned directly in any `Cargo.toml`), so `cargo update -p rustls` (0.23.42 → 0.23.45) was enough; no manifest edit needed. Full workspace test suite reconfirmed green after the bump.

## [1.9.41] - 2026-09-28

A generated schema reference doc (replacing a proposed migration squash — see the vault's Key Decisions for why), the 3 remaining pre-existing clippy warnings fixed, and Tier 0 of a new CI/CD framework.

### Added
- **`SCHEMA.sql`** — a `pg_dump --schema-only` snapshot of the current dev DB, checked in as a point-in-time reference. Not applied by `sqlx`, not part of `migrations/`; regenerate by hand after schema-changing migrations. Solves the "86 migrations is a lot to read to understand current shape" problem a full squash would have solved, without that approach's real risk of reconciling an already-applied migration history against a new file set.
- **`scripts/preflight.sh`** — Tier 0 of a new tiered CI/CD framework (`fmt`/`clippy`/`test`/version-consistency/secret-pattern-scan, run before pushing, never runs `#[ignore]`d real-DB tests so it never spends Neon compute). Full design in the vault's `reference/UnitPrep CI-CD Framework.md`.

### Fixed
- **The 3 pre-existing clippy warnings** (`sort_by`→`sort_by_key` ×2 in `dropbox_browse.rs`, an `#[allow(clippy::too_many_arguments)]` with justification on `edit_person_and_facility_link`'s genuinely-8-field form-edit signature) — the only things keeping `preflight.sh` from passing cleanly.

## [1.9.40] - 2026-09-28

An operations runbook, and the first step of a `client_ops` schema-modularity pass (prompted by a review finding the schema had accreted concerns beyond its own stated scope).

### Added
- **`RUNBOOK.md`** — required configuration, the four distinct session lifetimes and which are durable across a restart as of `v1.9.39`, the single-instance deployment assumption, and basic stuck-deploy recovery steps.

### Changed
- **`dropbox_configuration`/`process_street_settings` moved from `client_ops` to a new `integrations` schema.** Both have been gated by the `integrations.manage` permission (not `client_ops.perform`) since `20260909150000_admin_only_integrations_settings`, but their schema namespace never followed. `client_ops`'s own module doc comment defines that domain as "tooling an Onboarding Manager uses day to day" — `audit_log`, `tool_runs`, `vendor_format`, `qms_tag`, and `tag_pattern` all genuinely belong there by that definition and were left in place; this was a 2-table fix, not the sprawling reorganization it looked like from the outside. `ALTER TABLE ... SET SCHEMA` preserved RLS policies/triggers/indexes/FKs unchanged; `scripts/setup_app_service_role.sql` gained the matching schema-USAGE grant block.

## [1.9.39] - 2026-09-24

A durable session store, closing the sharpest P1 finding from an external code review: Group Prep/Dedup/Template Tagger sessions and WebAuthn passkey ceremonies were purely in-memory, so a restart or crash mid-upload (or mid-enrollment) silently stranded the user with no way to recover short of starting over.

### Added
- **`DurableSessionStore<S>`** (`core/src/durable_session_store.rs`) — a write-through wrapper around the existing `InMemorySessionStore<S>`. `get_handle` keeps returning the same shared `Arc<RwLock<S>>` for in-process concurrent callers (required by `SessionStore`'s own locking contract — a naive "deserialize fresh from Postgres on every call" implementation would silently let concurrent handles diverge); `save()` writes through to a new `auth.durable_sessions` Postgres table; a `get_handle` miss cold-hydrates from that table before falling back to normal `InMemorySessionStore` behavior. A second, independent periodic sweep expires stale Postgres rows, since the in-memory store's own sweep has no way to call back into the wrapper. Payload is bincode, not JSON — several of these session types carry raw `Vec<u8>` fields (WebAuthn ceremony state, Tagger's uploaded file bytes) that JSON has no compact representation for.
- **One shared, kind-discriminated table** (`auth.durable_sessions`, migration `20260924160000`) backs every session type this store holds, keyed by `(kind, id)` — not one table per kind, since every kind shares the exact same shape (an id, `SessionMetadata`'s small envelope, an opaque payload).
- **Applied to all four session types**: WebAuthn `RegistrationCeremony`/`AuthenticationCeremony`, and — after making each serializable through its full type graph — `unit_group_session::Session` (Group Prep), `DedupSession`, and `TaggerSession`. Group Prep's was the hardest: `SessionData` holds `Arc<Vec<CsvDocument>>`/`Arc<AnalysisResults>`, needing serde's "rc" feature; Dedup and Tagger's session types turned out to already be plain, easily-serializable data with no blockers.
- **Real-DB integration tests** for all four session types, each proving a session survives a simulated process restart: save it, drop the store, build a fresh one against the same pool, confirm `get_handle` rehydrates every field correctly.

## [1.9.38] - 2026-09-24

A standing modularity law and three fixes triggered by a follow-up codebase audit that caught the previous session's own `router.rs` growing to 1909 lines.

### Added
- **A standing modularity/file-size law in both repos' `CLAUDE.md`** — "check file size and concern-mixing after finishing a task, not only when starting one." The vault's own 250-line-module rule existed but was only reachable via on-demand recall and didn't re-trigger once a task's design work was done; the previous session's `GatedRouter` work had grown `router.rs` to 1909 lines with nothing catching it until this session's own audit.
- **Field-parity tests for `refresh.rs`** — `company_field_value`/`facility_field_value`/`facility_fields_that_differ`/`clients::create::diff_company_fields` are each a hand-written per-field list with no catch-all, so a field added to `MappedCompany`/`MappedFacility` but forgotten in one of them compiled fine and silently went unrecognized. New tests read the struct's own field names via `serde_json` instead of hand-listing them a second time, so they actually catch drift (`go_live_date` deliberately excluded, matching the module's existing doc comments).

### Changed
- **`router.rs` split, 1909 → 3 files**: `router/routes.rs` (the route table), `router/permission_gate_tests.rs` (the `#[cfg(test)]` proof the table is enforced), `router/mod.rs` (the public entry point, response-shaping middleware, rate-limit error handling) — pure reorganization, `build()`'s behavior unchanged.
- **Policy-edit handlers DRY'd up**: `fees.rs`/`taxes.rs`/`delinquency.rs`/`coverage.rs` (tiers and commission) each hand-wrote the same "count existing rows, then delete them all" pair before their own insert loop; factored into one shared `count_and_delete_existing(tx, table, facility_id)` helper. `specials.rs` was left alone — it's a single-row `ON CONFLICT` upsert, not the delete-then-reinsert shape the others share.

## [1.9.37] - 2026-09-24

### Added
- **`GatedRouter`**, closing a gap `THREAT_MODEL.md` had named explicitly: once roles/permissions moved off the closed `Role` enum onto data-driven tables, the compiler could no longer catch a new handler that forgot to call `require_permission`. `GatedRouter` wraps `axum::Router` and never re-exposes `.route()` — every route must declare its `RouteAccess` (`Public | AuthCeremony | Authenticated | RlsRead | RlsWrite | Permission { keys, action }`) at the call site. A new `permission_gate_tests` module calls the real handler behind every `Permission`-classified route with a zero-permission caller and asserts 403, so a handler that declares `Permission` but forgets the real check fails a test, not just a manifest check. Classifying all 106 routes against real handler source (not `router.rs`'s own comments) found one stale comment (`/integrations/process-street/settings` GET was documented as open but actually gates on `integrations.manage`) and a genuine pre-existing test gap (`auth_configuration.rs`'s two handlers had zero tests of any kind).
- **`ts-rs`-generated frontend types** for the four core tool-session response families (`UploadResponse`, `DiscoverResponse`, `ValidateResponse`, `AnalyzeResponse` plus their transitive dependencies, 11 types total) — replacing hand-mirrored TypeScript that had already drifted silently once (an `output_path` field removal broke at runtime with nothing catching it). `npm run generate-types` regenerates the file; `i64` fields default to `bigint` except where overridden (`UnitFileCandidate.modified_at`, a real epoch-millis `number`, tagged `#[ts(type = "number | null")]`).

### Fixed
- **Client-ops audit log recorded before commit, not after, in 13 handlers** (`fees.rs`/`taxes.rs`/`delinquency.rs`/`coverage.rs`/`specials.rs`, 3× `clients_facility_people.rs`, 2× `clients_companies.rs`, `clients_dropbox_folder.rs`, `tool_runs.rs`) — `client_ops::audit_log::record` writes on a separate connection from the handler's own RLS transaction, so recording before `tx.commit()` meant a commit failure after a successful audit write left a permanent "X updated" row for a change that never landed. Reordered to commit-then-audit, matching the pattern `clients_elavon.rs`/`clients_manual_link.rs` already used correctly.

## [1.9.36] - 2026-09-23

### Added
- **Onboarding Summary tab** on the Company page — one row per facility showing Elavon Status (the next outstanding step in that facility's Merchant Account workflow, walking `clients.ps_task_status` up to and including "Add Credentials to QMS," with everything after that step ignored as PS-internal follow-up) and a Duplicate Checks count linking to the facility's own Onboarding Work tab.
- **Delete action for a mistaken Onboarding Work tool run** — `client_ops.tool_runs` shipped append-only with no way to clear a run logged by mistake (e.g. a Dedup check run against the wrong facility's uploaded data); new `DELETE /clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}`, gated by a new role-scoped RLS policy, writes a `tool_run_deleted` audit row.
- **Manual Link action** on the Company page — relinks a facility's Intake or Merchant Account record to a different Process Street run in place, including *over* an already-linked run (unlike the Elavon tab's own "Link Manually," which only ever handled the unlinked case). Built after a real client (Knapp's Self Stor of Milton Freewater) was found linked to a completely unrelated business's Merchant Account application, copied by hand from an undisambiguated search result.
- **EIN and address disambiguation on Merchant Account search results** — `MappedMerchantAccount` gained masked `ein_last_4` and a combined `business_address`; a new fuzzy address-matching helper canonicalizes street-type abbreviations. "Potential Duplicates" groups now show a per-group `addresses_agree` signal, and a new near-miss check flags a Merchant Account match whose title shares real vocabulary with a facility match without being an outright substring match, surfaced as a "Similar name to…" warning. Decision-support only — nothing auto-resolves a correlation.

### Changed
- **Resync `apply` no longer re-fetches Process Street from scratch after `preview`** — both phases independently called the same live-fetching comparison function, so confirming a resync (even a no-op "keep everything") cost as much as the preview itself. `preview_resync` now caches its own fetch (5-minute TTL, keyed by company); `apply_resync` drains it when still fresh, falling back to a live fetch only when the cache has nothing usable.

## [1.9.35] - 2026-09-22

### Fixed
- **Per-client Re-sync never refreshed Elavon/Merchant Account data** — `clients.facility_merchant_accounts` and `clients.ps_task_status` for the `merchant_account` workflow previously only refreshed via the Elavon tab's own dedicated "Resync Elavon Data" button, so a step unchecked in Process Street after a facility's last Elavon-specific resync stayed stale in OO indefinitely. `apply_resync` now also looks up every facility's linked Merchant Account run and refreshes it, concurrently with the Intake fetches it already made.

## [1.9.34] - 2026-09-22

### Added
- **Daily-time Process Street sync schedule** — `schedule_mode`/`sync_time`/`sync_timezone` (a closed IANA-timezone list, DST-aware), alongside the existing interval-based schedule.

### Fixed
- **Vendor-format registry's background refresh widened from 5 minutes to 4 hours** — root-caused a Neon free-tier compute-usage alert to this loop's 300-second poll colliding almost exactly with Neon's Free-plan default 300-second idle-suspend timeout, so a running server's compute never got a real idle window to suspend in. Process Street's own sync was ruled out as a cause first (its production branch showed zero compute time for the whole period).

## [1.9.33] - 2026-09-22

### Fixed
- **Dedup vendor-detection parse failures now logged, and non-UTF-8 CSVs tolerated** rather than failing silently or erroring outright.

### Added
- **`ps_person_index` refreshed during per-client Re-sync** — the Re-sync button previously only refreshed `clients.companies`/`clients.facilities` row fields, never the person-search index the Users tab's "Add User" candidate chips are sourced from, so a person added in Process Street after a facility's initial import never appeared as a candidate until the next scheduled sync.

## [1.9.32] - 2026-09-18

### Added
- **`Business_DBA` as a second Merchant Account correlation signal**, additive alongside the existing run-title parenthetical — real Process Street data has a second, plain naming convention (`"<name> - New Elavon Account"`, no parenthetical at all) that the original title-matching path could never correlate.
- **Force mode for the manual sync trigger** (`?force=true`) — bypasses the delta check entirely, treating every run as never-synced. Built to backfill the new `business_dba` column onto ~363 already-indexed rows; the actual backfill run was deliberately deferred as not worth the Process Street API budget against no active problem.

## [1.9.31] - 2026-09-15

### Changed
- **Dedup exports now use facility-scoped, versioned filenames.**

## [1.9.30] - 2026-09-15

### Added
- **Merchant Account run titles searched alongside Intake** in the Add-from-Process-Street flow.

## [1.9.29] - 2026-09-15

### Added
- **Implementation Manager / Sales Rep company assignments.**

### Changed
- **State filter values normalized.**
- Applied `cargo fmt` across the workspace.

## [1.9.28] - 2026-09-11

### Added
- **Onboarding Work tab backend**: `client_ops.tool_runs`, a durable, queryable per-facility record of every tool run (Dedup first; `unit_groups`/`template_tagger` to follow once those tools are wired up the same way). `facility_id` is `NOT NULL` — a run cannot exist unrelated to a facility. The source file's own bytes are stored alongside its Dropbox path, since a Dropbox-sourced file can be moved or deleted out from under a stored path. Distinct from `client_ops.audit_log`, which stays a generic, facility-blind event trail. Dedup/Unit Groups/Template Tagger routes moved from client-scoped to facility-scoped URLs to match.

### Fixed
- **`attach_output_bytes`/`attach_output_dropbox` silently updated zero rows** — both ran their UPDATE against the raw connection pool with no RLS GUCs set, and Postgres requires an updated row to satisfy both the UPDATE policy and the table's own SELECT policy; without `app.current_user_id` set, every UPDATE silently affected nothing, so a run's output was never attached after export, with no visible error. Fixed by moving both onto `begin_rls_transaction`, matching every other write in the codebase.

## [1.9.27] - 2026-09-10

### Fixed
- **A short, single-word parenthetical nickname could false-match an unrelated client's Merchant Account run** — a facility named `"...(Main)"` matched an entirely unrelated new client whose own Intake title happened to contain the plain word "main," and nothing about the match was ambiguous enough to be rejected. `is_specific_enough(keyword)` now requires a nickname to be either multi-word or 6+ characters to be trusted as a candidate at all; a nickname too short is excluded entirely, the same treatment as no parenthetical existing.

## [1.9.26] - 2026-09-10

### Added
- **A `developer` system role**: full `client_ops`/integrations access, no security-config/security-log access unless `admin` is also assigned, plus everything `onboarding_manager` has. Since this codebase's RLS policies hardcode role checks directly rather than checking permissions, granting `developer` the right permissions alone would have passed the app-layer check and then silently failed at the database — closed by introducing one shared `auth.current_user_is_client_ops_role()` function and repointing all 69 existing role-gated policies (across 23 tables) at it, so extending the club is now a one-line change instead of a 69-policy sweep.

## [1.9.25] - 2026-09-10

### Added
- **QMS Credentials / Pin Pad Credentials** section on the Elavon tab — 4 fields (Account ID, PIN/Password, Pinpad User ID, QSS API Pin) that were already being fetched and encrypted into `facility_merchant_accounts.encrypted_secrets` since the original Elavon tab shipped, just never decrypted back out for display.
- **CLAUDE.md warning against Windows/Dropbox/OneDrive clones** of either repo — this codebase exists only in WSL.

### Changed
- **The Elavon tab's credentials-only resync redesigned into one full-tab resync.** The narrow "Resync Credentials" button shipped a day earlier deliberately never touched `credentials_added_to_qms` (fetched via a separate call), so there was no way to refresh that field short of a destructive unlink/relink. Replaced entirely with one "Resync Elavon Data" button that refreshes rate, status, `credentials_added_to_qms`, financials, credentials, and parties together — safe as a full overwrite since nothing in Merchant Account data has a manual-edit UI in OO to clobber.

## [1.9.24] - 2026-09-09

Closes out the admin-only Integrations settings work from earlier the same day, plus an independently-verified follow-through on an external codebase review: 5 refactors (2 god-file splits, a shared response-helper consolidation) and a full README rewrite.

### Added
- **Admin-only Integrations section**: a new `integrations.manage` permission (admin-only — `admin` deliberately never holds `client_ops.perform`, the permission Process Street settings had been gated on, so the two couldn't share a gate). A companion RLS migration moved Process Street settings' write policy off `onboarding_manager`/`department_manager` onto `admin` too, since the app-layer permission check alone isn't the real gate. Real behavior change: `onboarding_manager`/`department_manager` lose the Process Street settings access they previously had.
- **Editable, encrypted Dropbox settings**: the five `DROPBOX_*` env vars became a singleton `client_ops.dropbox_configuration` table, admin-only at the RLS layer. `app_secret`/`refresh_token` are ChaCha20-Poly1305-encrypted under their own dedicated key, never `CLIENT_PII_ENCRYPTION_KEY` or `TOTP_ENCRYPTION_KEY`. The API never returns decrypted secrets, only `has_app_secret`/`has_refresh_token` booleans. `main.rs` tries the DB row first, falling back to env vars — not live-reloaded, a saved change takes effect on the next restart.
- **Audit logging on facility-person and client-archive actions.**

### Changed
- **Response-helper consolidation**: 14 files' worth of hand-rolled `bad_request`/`not_found`/`conflict` literals collapsed into 3 shared functions in `api::mod`.
- **`clients_facility_policies_edit.rs` split** (965 → 5 files, one per policy category) and **`clients/sync.rs` split** (1422 → `progress.rs`/`refresh.rs`/`orchestrator.rs`).
- **`README.md` rewritten** to reflect the current platform — the prior version still described a two-tool, no-auth early product; the real system has enforced passkey/TOTP auth, RBAC, and a Process Street-sourced client platform. `dedup/RULES.md`'s stale `relatedness.rs` module pointer (now a directory) corrected.
- Two claims from the external review that prompted this pass didn't hold up on independent verification and were corrected rather than acted on as given: only 3 of a claimed 8 files actually shared the same editing/saving/error state machine, and `repository.rs`'s 1404 lines were ~50% test code, not a real god-file.

## [1.9.23] - 2026-09-08

### Fixed
- **CORS never allowed `DELETE`** — `CorsLayer::allow_methods` listed only `GET/POST/PUT/PATCH`, so every cross-origin DELETE request (not just the new client-delete endpoint below — every existing DELETE route, including facility-person unlink, Elavon unlink, and role revocation) was silently refused at the browser's own preflight step, indistinguishable from the server being down. Added `DELETE` to the allowed list, plus a real HTTP-level regression test confirmed to fail without the fix.

## [1.9.22] - 2026-09-08

### Added
- **`DELETE /clients/{company_id}`** — permanent client delete, distinct from the existing archive action, cascading to every facility/policy/link row via existing FK `ON DELETE CASCADE` declarations. Never touches `clients.people` itself, since a person can be linked under other companies too.

## [1.9.21] - 2026-09-08

### Fixed
- **Person identity was keyed on email alone**, silently collapsing distinct people who share one inbox — a real family case had several genuinely different owners sharing one inbox address, so only the first owner's insert into a given `(facility, person, role)` succeeded and every later same-email owner's insert was silently dropped by `ON CONFLICT DO NOTHING`. `clients.people` identity is now `(email, full_name)`, both case-insensitive. New `heal_person_in_place(tx, person_id, full_name, phone)` corrects a known roster row directly by its own id, splitting "resolve identity for a new Add" from "correct a name that's already wrong" — the two needed opposite matching behavior to both be right at once.

## [1.9.20] - 2026-09-08

### Fixed
- **Company legal name wasn't resolved when a Merchant Account form answered "Same as Business DBA"** — Process Street never asks for the legal name in that case, so the old resolution logic fell straight through to Intake's own (often blank, for a non-first-time sister facility) legal name. Added an explicit branch that uses the facility's `Business_DBA` value instead.

## [1.9.19] - 2026-09-08

### Added
- **DropBox tab**: lets a manager relink a facility to a different Dropbox folder (or clear it), audit-logged the same way as a Merchant Account relink, and protected from re-sync overwrite via the existing `manually_edited_fields` mechanism.

## [1.9.18] - 2026-09-08

### Added
- **Source tracking, manual add, and full editing for facility people.** `clients.facility_people` gained a `source` column (`process_street` | `manual`); a manually-added person is permanently exempt from the Users tab's self-heal pass. Every roster row became directly editable, with a `protect_from_resync` checkbox that flips a Process-Street-sourced person to `manual` so an edit survives the next self-heal instead of being silently reverted.

## [1.9.17] - 2026-09-08

### Added
- **Structured Taxes and Delinquency data**, replacing free text: `clients.policy_tax_entries` (name/description/flat amount/attribute-payable percent/recurring flag) and `clients.policy_delinquency_entries` (a dollar amount, an optional days-after count, and a trigger referencing either the facility's Paid Through Date or another entry on the same schedule by category). Old free-text tables weren't dropped or auto-migrated — real historical data needs a human's judgment to convert, so both tabs show legacy data alongside the new structured entries when a facility has it.

## [1.9.16] - 2026-09-04

### Changed
- **Facility Policies split into 5 editable tabs** (Fees / Taxes / Delinquency / Coverage / Specials).

### Added
- **QSX sync exemption** for Facility Policies.

## [1.9.15] - 2026-09-04

### Added
- **Unlink for facility people**, plus self-heal now runs on page load instead of only on click.

## [1.9.14] - 2026-09-04

### Added
- **Facility Users tab**: roster plus Process Street candidate chips for adding a person.

## [1.9.13] - 2026-09-04

### Fixed
- **Client-ops audit log FK violation on system-triggered events.**

## [1.9.12] - 2026-09-04

### Added
- **Dropbox import/save pattern extended to Unit Groups (Group Prep).**

## [1.9.11] - 2026-09-04

### Added
- **Dropbox import/save pattern extended to the Template Tagger.**

## [1.9.10] - 2026-09-04

### Fixed
- **A facility's Dropbox folder is now resolved from its own captured link** rather than guessed.

## [1.9.9] - 2026-09-04

### Fixed
- **Dedup no longer guesses a facility's Dropbox folder from a single search candidate.**

## [1.9.8] - 2026-09-04

### Added
- **Dedup's Dropbox folders default to the client's real folder**, and its save-to-Dropbox flow defaults to a Duplicate Check subfolder next to the source.

## [1.9.7] - 2026-09-04

### Added
- **The Add-to-OO confirmation screen can assign facility People from a shared pool** instead of only per-facility.

## [1.9.6] - 2026-09-03

### Fixed
- **Facility search now shows every facility's own people**, not just ones matching the query text.

## [1.9.5] - 2026-09-03

### Fixed
- **Person-name search fixed for a dash-separated name/contact Process Street format.**

## [1.9.4] - 2026-09-03

### Added
- **`website_url`**, threaded through mapping, create, and re-sync.

## [1.9.3] - 2026-09-03

### Added
- **Unlink action for a facility's Merchant Account link.**

## [1.9.2] - 2026-09-03

### Added
- **Each Intake run's "first time" answer surfaced** for company-source picking.

## [1.9.1] - 2026-09-03

### Added
- **EIN, masked bank routing/account, and revenue/volume shown on the Elavon tab.**

## [1.9.0] - 2026-09-03

Phases 3-5 of the Process Street integration: the Add-to-OO confirmation screen, a hybrid two-phase re-sync with a new Activity Logs feature, and the first read-only pass of the client record UI (Company page, facility rail, Elavon tab, Facility Policies).

### Added
- **Add-to-OO confirmation screen**: Company section plus one section per selected facility, pencil-edit-in-place per field, `POST /clients` on Create. Company is its own section, not a role a facility switches into — every selected Process Street run becomes its own facility row, including whichever one also seeds the Company section's data (reversing an earlier either/or design that didn't match a real client's actual shape). Create latency fixed by batching and concurrently fetching every needed Process Street run instead of sequentially, cutting an ~18s Create down substantially.
- **Re-sync with hybrid conflict resolution**: a configurable background scheduled sync plus a manual Re-sync button, both landing through a two-phase preview/apply flow — `preview_resync` classifies each changed field as a safe auto-apply or a conflict against a new `manually_edited_fields` tracking column, `apply_resync` takes the caller's per-field resolution for each conflict.
- **Activity Logs** (`client_ops.audit_log`, `/admin/activity-logs`), a new user-action trail distinct from the renamed **Security Logs** (was Audit Logs) — captures every sync run including failures, plus other user actions, gated on its own `activity_logs.read` permission.
- **Client record UI, read-only pass**: Company page (Company Information, Financial Information, Owner(s) Information with per-party PII decryption), a facility rail, and a facility's General/Elavon/Facility Policies tabs (Users/DropBox tabs shown as placeholders). The Elavon tab includes an auto-suggested Merchant Account candidate via title correlation, with a deliberate manual "Confirm this link" step — never auto-accepted.

### Fixed
- **Elavon data was never actually being written** — the Merchant Account run id resolved during search preview was silently dropped before Create, so `ingest_merchant_account_run`/`insert_party` (built in Phase 1) were never invoked for anything created through the confirmation screen.
- **Page "blink" on facility switching** — `get_company_detail` (4 sequential round trips) and `get_facility_policies` (up to 7) now run their queries concurrently in separate short-lived RLS transactions instead of one shared sequential transaction; the frontend stopped re-fetching company detail on every facility click via a new `CompanyDetailContext`.
- **Dropbox link replaced with a single "Go to DropBox" button** opening the web share link in a new tab — there is no local filesystem path in this data for a desktop-Explorer button to use.

## [1.8.21] - 2026-08-31

### Added
- **Add-to-OO backend foundation** (Phase 3): `clients::company_naming::resolve_company_name` (Merchant Account's typed Legal Name, then "same as DBA," then Intake's own legal name, with a constructed `"<Owner> DBA <Business_DBA>"` form for a sole proprietor), `clients::create::create_company_and_facilities` — the real trigger behind `POST /clients`. `clients::repository` split into reusable `insert_company`/`insert_facility`/`insert_facility_policies_and_people` building blocks.
- **Facility search narrowed to Intake runs only**, with `already_imported` flagging per match — Merchant Account existing or not isn't a reliable thing to search by, since not every client uses Elavon.
- **Merchant Account's Legal Name / DBA / Ownership Type fields captured** from the Facility Information (Pre-App) step.

## [1.8.20] - 2026-08-31

### Added
- **Manual "Sync Now" trigger** with live progress, plus a real configurable schedule (replacing an earlier fixed-daily-time design).

## [1.8.19] - 2026-08-31

### Added
- **QMS subdomain and system-email columns** on companies/facilities, mapped and written from Intake.

## [1.8.18] - 2026-08-31

### Added
- **A real `ProcessStreetClient` wired into the running app**, gated on config, plus the combined `/clients/search` endpoint over both facility-name and person-name search paths.

## [1.8.17] - 2026-08-31

### Fixed
- **`Signer_Name` pointing at an existing owner is not a second role** — no longer double-counted.

## [1.8.16] - 2026-08-31

### Added
- **Delta-aware background sync feeding a Process Street person-search index** — `clients.ps_sync_state`/`clients.ps_person_index`, since Process Street has no server-side search over form-field values. Tracks Process Street's own run-update timestamp per run to decide when to re-index, rather than re-pulling every run on every cycle.

## [1.8.15] - 2026-08-31

### Added
- **Server-side company/facility-name search** across all three Process Street workflows, using Process Street's own real (if undocumented) `name` filter — no pre-sync/cache needed for this half of search. Narrowed to Intake-only later the same day (see v1.8.18) once it became clear searching Merchant Account/Contract Order too made "no match there" look like "doesn't exist."

## [1.8.14] - 2026-08-31

### Added
- **`ingest_facility` proven against the live Process Street API**, not just fixtures.

## [1.8.13] - 2026-08-31

### Added
- **Contract Order Process Street workflow mapping** (`migrating_from_system`, the one field flagged as operationally important — the rest of that workflow's ~99 fields stay recoverable from the raw snapshot). Wired into the repository and ingestion trigger.

### Fixed
- **`list_workflow_runs` only ever searched `status=Active` runs** — Contract Order runs are marked `Completed` once processed, so the original search silently found none of them for either of the two real clients used to build this mapping. Now queries Active + Completed + Archived and merges the results; this also revealed a Contract Order run for a facility an earlier, narrower search had missed entirely.

## [1.8.12] - 2026-08-31

### Added
- **Field-level encryption for Process Street's sensitive Merchant Account data** — pulling that workflow's full field list (not just the handful of fields first checked) surfaced real SSNs, DOBs, home addresses, EINs, and bank routing/account numbers for real client owners. `facility_merchant_accounts.encrypted_secrets` and a new `facility_merchant_account_parties` table (signer + up to 4 owners + up to 4 intermediary businesses), encrypted the same way `auth::totp` already is: ChaCha20-Poly1305, a version-prefixed blob, with the AEAD's additional-authenticated-data binding each ciphertext to the specific row it belongs to. Both new/touched tables' SELECT RLS tightened to `onboarding_manager`/`department_manager` only, not the blanket "any authenticated" every other `clients` table uses. The raw snapshot column excludes every sensitive key outright, not just the ones re-encrypted elsewhere.
- **Process Street field mapping for the Intake/Progress and New Merchant Account workflows**, plus the `clients` schema's repository layer and a full per-facility ingestion trigger (`clients::ingest::ingest_facility`).

### Fixed
- **Missing sequence grants for `app_service` on the `clients` schema** — `GRANT ... ON ALL TABLES` doesn't cover a sequence, a separate grantable object; found via a real end-to-end integration test round-tripping a golden fixture through real Postgres.

## [1.8.11] - 2026-08-28

### Added
- **A read-only Process Street API client** (`src/process_street/`), mirroring the existing Dropbox client's shape — pagination-following helpers for `/workflows`, `/workflow-runs`, tasks, and form-fields. No write methods exist, by design; Process Street access is a live ops system the onboarding team depends on daily.

## [1.8.10] - 2026-08-28

### Added
- **A new `clients` schema** for the Process Street integration — companies, facilities, facility policies (Fees/Taxes/Delinquency/Coverage/Commission/Specials, each owned 1:1 by exactly one facility, shared across sister facilities only via an explicit per-category copy action, not an implicit database relationship), people, merchant accounts, contract orders, and generic Process-Street task-status tracking. Kept separate from `client_ops` (tool-support/reference data) since this is a much larger, faster-growing domain that will need its own access-control boundary once client-scoped visibility ships. RLS scoped the same way `client_ops` already is: any-authenticated read, `onboarding_manager`/`department_manager` write — verified live, not just by policy count.

## [1.8.9] - 2026-08-17

A broad batch spanning a generalized vendor-format registry, a colleague cross-check's follow-on dedup fixes, and the first Dropbox integration.

### Added
- **`client_ops.vendor_format`**, a shared DB-backed registry generalizing what had been hardcoded, per-tool vendor recognition (QSX/Storage Commander/DoorSwap for units, QSX for tenants) into one module read by both Group Prep and dedup — built to onboard a real Easy Storage Solutions tenant export. Cached in `AppState` with a 5-minute background refresh, never queried per HTTP request. A new "prefer data over hardcoding" design principle recorded in both repos' `CLAUDE.md`.
- **`POST /dedup/detect-vendor`** — a pre-Run-Check gate showing the detected vendor with a confirm checkbox, mirroring Group Prep's existing recognize-then-confirm flow.
- **Dropbox integration for the QMS Onboarding folder** — folder search, sorted `/dropbox/list` results, a folder-picker that filters out files, and real Dropbox read/write wired into Dedup.
- `first_name`/`last_name` now returned from `/auth/whoami`.
- A manual unit-file upload override for unrecognized vendor formats, and QMS registered as a recognized units vendor format in its own right.

### Fixed
- **A regressed "None"-style placeholder bug**: a placeholder value (`"None"`, `"N/A"`, ...) was no longer being treated as blank in flagged-group comparisons — found by a colleague's independent skill review of a real Westpark run. Rescoped to `FieldKind::Plain` fields only, preserving the existing rule that a garbage Phone/Address value still counts as a real mismatch against blank.
- **Typo-variant candidates now name which categories actually differ**, instead of a bare matches/differs boolean.
- **Dedup XLSX export formatting**: the 255-character column-width blowout, no wrap/freeze/autofilter, and unmerged section-banner rows were all fixed; a CSV-injection mitigation that produced a visible artifact with zero security value in a real `.xlsx` (which carries its own type metadata) was removed from the XLSX writer, kept unchanged for the CSV writer where it's genuinely needed.

### Backend logging/observability sweep.

## [1.8.8] - 2026-08-14

### Fixed
- **Duplicate `e.zip` tag_key removed** from the QMS tag catalog.

### Added
- **Label-adjacent value recognition in already-filled documents**, and label patterns seeded from a real filled rent late-notice letter.

## [1.8.7] - 2026-08-14

Milestone 8 (final) of a third CTO-grade audit's fix plan — the remaining low-severity polish items.

### Fixed
- A stale doc-comment dead link, a shadowed closure parameter, an avoidable clone on every row in `RowScan::group_fingerprint`, and a redundant re-fetch-and-clone right after `DedupSessionService::create_session`.
- **Audit-log PDF rendering wrapped in `spawn_blocking`** — the synchronous, CPU-bound render was blocking the async runtime.
- **`login_begin`/`register_begin` now capture IP on their audit-log rows** — a prior "no ConnectInfo on this leg" decision was deliberately reversed, for better probing-attempt correlation.
- Documented, rather than mitigated, `login_begin`'s residual timing side-channel (real WebAuthn-challenge work happens only when a login candidate resolves) — the timing delta is small relative to the DB round trip every branch already pays, and there's no cheap no-op challenge to build instead.

## [1.8.6] - 2026-08-14

### Fixed
- **Capture IP on `/begin` handlers' audit-log rows.**

## [1.8.5] - 2026-08-14

Milestone 7 of the third audit's fix plan — closing two remaining test-coverage gaps.

### Added
- Round-trip and missing-column tests for dedup's `ingest.rs`, and direct tests for `tagger.rs::check`'s two DB-free branches.

### Fixed
- A stale doc-comment reference, a shadowed closure parameter, and an unnecessary clone in `DedupSessionService::create_session`'s caller.

## [1.8.4] - 2026-08-14

Milestone 6 of the third audit's fix plan — 10 large files split along genuine seams (6 backend, 4 frontend), following Milestone 5's DRY pass.

### Changed
- **Backend**: `docx-surgeon/src/edit.rs` (665 lines) → `edit/{mod,fragment,run_xml,overlap}.rs`; `src/api/mod.rs` + `auth_audit_logs.rs` split together into `state.rs`/`router.rs`/`health.rs`/`auth_audit_logs_export.rs` (a real cross-file dependency forced these two into one commit); `resolve_unit_format.rs`'s confirm/manual-mapping logic moved into `discover/format_resolution.rs`; `audit_log_pdf.rs` → `layout.rs` + `render.rs`; `dedup/src/relatedness.rs`'s union-find household grouping split into `relatedness/household.rs`.
- **Frontend**: `lib/auth.ts` (694 lines) split into 5 focused modules (`auth-shared`/`auth-session`/`auth-users`/`auth-audit`/`auth-config`); `admin/users/page.tsx` (805 lines) split into `page.tsx`/`InviteUserForm.tsx`/`UserRow.tsx`/`useUsersAdmin.ts`/`styles.ts`; `MasterGroupFileSection.tsx`'s manual-upload flow extracted into its own hook; `WarningsSection.tsx`'s per-reason-card JSX extracted into `WarningReasonCard.tsx`.

## [1.8.3] - 2026-08-14

Milestone 5 of the third audit's fix plan — DRY consolidation across both repos.

### Changed
- Shared `session_lifetime_hours()`, `request_context()`/`user_agent_from()` helpers (used across ~17 call sites), and shared audit-log filter-building functions.
- **`assign_tiers` rewritten from an O(n²) nested scan to a single-pass `HashMap` count**, plus a new 2000-candidate hard cap and a tagger-specific 10MB body-size cap.
- Shared `blank_aware_key`/`blank_last_sort_key` helpers in dedup's `comparison.rs`, reused by `phrasing.rs`.
- Frontend: a shared `useFileUploadAction` hook (adopted by 3 upload pages, 2 of which previously had no session-expiry handling at all), a shared `useAuditLogFilterData` hook, and a previously-silently-swallowed QMS tag catalog fetch failure now surfaced as a real banner.

## [1.8.2] - 2026-08-13

Milestone 2 of the third audit's fix plan.

### Added
- **Passkey-based step-up gating self-service TOTP re-enrolment** — the real gap the audit's TOTP finding pointed at (admin-driven onboarding TOTP setup was never the issue; self-service re-registration having zero re-authentication was). Modeled directly on TOTP's own existing step-up mechanism, reusing the same WebAuthn ceremony machinery, as its own bespoke mechanism rather than folded into the existing TOTP-specific step-up config.

### Fixed
- **Invite-time role assignment now requires `users.manage_roles`**, not just `users.manage`, closing a latent privilege-escalation seam.
- **TOTP re-enrollment's wrong-code branch now respects lockout**, matching step-up's existing lockout behavior.
- **A manual group-file upload's hand-rolled fetch now treats a 401 the same as a 404** (session-expired), instead of surfacing a raw error.

## [1.8.1] - 2026-08-13

Milestone 1 of a third CTO-grade audit's fix plan — the audit's 4 highest-risk findings.

### Fixed
- **Session-ownership IDOR**: every session-touching handler switched from the unowned `with_session`/`with_session_mut` to the already-correct-but-unused `with_owned_session`/`with_owned_session_mut` — including `tagger.rs`'s report/apply handlers, a second real instance the original audit's own agent-scoping had missed entirely.
- **`docx-surgeon`'s `quick-xml` CVE**: bumped 0.36 → 0.41, closing 2 RustSec advisories reachable via uploaded `.docx` files. Required rewriting `<w:t>` text extraction from a single-event read to a loop, since 0.41 splits entity/character references out of `Event::Text` into a new `Event::GeneralRef` — a real API-shape change, not a drop-in patch.
- **Last-active-admin concurrency race**: the check-then-act admin-count guard had no row lock; a plain `FOR UPDATE` on the count query wouldn't have actually closed it either (two callers excluding different admins never lock the same row) — fixed by locking the shared `admin` role row itself, which genuinely serializes both callers.
- **Frontend admin-redirect race**: `app/(app)/layout.tsx`'s loading guard never accounted for `checked` still being `false`, so `RequirePermission.tsx` could mount with `user=null` on every fresh load and silently bounce a legitimate admin hitting an admin URL directly.

## [1.8.0] - 2026-08-11

Continued growing the QMS tag catalog from real-world evidence, and
shipped the write-side counterpart to the Phase 2 matching engine.

### Added
- **`e.dob`, `e.add1`, `e.add2`** — date of birth and the two-line
  address variant, confirmed directly against the live QMS tag
  picker. `e.address` (single-line) was already seeded.
- **`m.indate`, `m.secdep`, `l.indate`, `l.secdep`, `d.now`,
  `d.nowlong`** — identified from a real sample lease (Affordable
  Storage) for the QMS Template Tagging Assistant effort; each key
  independently confirmed in the vault's own transcribed tag-family
  notes before being added.
- **98 more tags**, harvested by scanning every `{{tag}}` occurrence
  across 258 already-tagged real client documents (the full QMS
  Onboarding document tree). Introduces six categories beyond the
  original Tenant/Unit/Lease/Move-In/Date-Time set: Alternate Contact,
  Military, Facility, Company, Vehicle, Lienholder, Signature. The
  catalog is now 121 tags, up from 13.
- **`docx-surgeon`**, a new standalone crate: surgical, minimal-diff
  text editing inside a `.docx` file. Given a document and a set of
  exact text-span edits, produces a new document where only those
  spans change — every other run, table, style, and zip part is
  copied through unchanged. Never deserializes the whole document
  into an object model; edits are spliced directly into the original
  XML bytes at each targeted run's own text range, with a decode/
  re-encode round trip that makes it impossible to corrupt a run
  whose text contains an XML entity. Proven against a real sample
  document, not just synthetic fixtures: every zip entry other than
  `word/document.xml` verified byte-identical after an edit, and
  everything in `document.xml` outside the one targeted run's text
  verified byte-identical too. Not yet wired into the template-
  tagging pipeline or any HTTP endpoint.

## [1.7.0] - 2026-08-10

Phase 1 of the QMS Template Tagging Assistant shipped: `client_ops`, the
first Postgres schema outside `auth`, holding a hand-maintained reference
catalog of QMS's document-template merge tags — a stand-in until QMS
exposes its own tag list via its own API. Seeded with the 13 tags QMS's
own Default Lease document calls out as its "popular variables," not the
full ~300+ tag catalog — growing this, and adding real context-scoping
(a tag can be valid in some document contexts and not others), is
tracked follow-up work, not a rework of what shipped here.

### Added
- **`client_ops.qms_tag`**: `tag_key` (natural key, matched against
  literal `{{tag_key}}` text in a document — never renamed), `label`,
  `category`, `is_active`. Never hard-deleted: deactivate/reactivate
  only, so a template already referencing a tag stays resolvable, or at
  least visible as deactivated, rather than disappearing outright.
- **`GET/POST /client-ops/qms-tags`, `PUT /client-ops/qms-tags/{tag_key}`,
  `PATCH /client-ops/qms-tags/{tag_key}/deactivate`,
  `PATCH /client-ops/qms-tags/{tag_key}/reactivate`.** Read is open to
  any authenticated caller (catalog/reference data, nothing sensitive);
  every mutation requires the new `client_ops.manage_tags` permission
  and writes a `client_ops.audit_log` row.
- **`client_ops.manage_tags` permission**, granted to `admin`,
  `onboarding_manager`, and `department_manager` alike — deliberately
  not the same shape as `client_ops.perform`, which `admin` does not
  hold. Maintaining a reference catalog of tag names reads as system
  configuration, not a client operation, so `admin` shares this one
  without blurring that boundary.
- **`client_ops.audit_log`**: a distinct, non-security operations trail
  for client-ops mutations (today: `qms_tag` edits; later: client
  credential adds/revokes and whatever else this domain grows). Kept
  separate from `auth.auth_audit_logs` on purpose — the same
  access-boundary split already locked between Admin's oversight audit
  and client-ops's own business data.

## [1.6.0] - 2026-08-07

Phase II (hardening) items 2, 4, 6, and 7 shipped: session/TOTP
hardening, anomaly/risk-based login signals, a formal threat model, and
audit retention/review documentation. Item 8 (ceremony-state
horizontal-scaling fix) scoped and deferred the same day — see
THREAT_MODEL.md. Phase II is closed out; nothing left on it is
scheduled, only trigger-gated.

### Added
- **Idle session expiry.** `auth.resolve_session` now takes a
  `p_idle_minutes` argument and refuses a session whose `last_seen_at`
  is older than that window (`SESSION_IDLE_TIMEOUT_MINUTES`, default
  30), independent of the existing absolute expiry
  (`SESSION_LIFETIME_HOURS`, default 12h, unchanged).
- **TOTP replay window.** `auth.totp_credentials` gains
  `last_used_step`, the TOTP time-step last accepted for that
  credential. `auth::totp::verify_code` now matches a submitted code
  against a specific candidate step (rather than trusting an opaque
  yes/no) and refuses one matching that step or an earlier one, closing
  the window where an observed code stayed replayable for the rest of
  its ~90s skew window.
- **Anomaly/risk-based login signal.** A login from an IP address or
  `user_agent` never seen before for an account with prior session
  history is now flagged: recorded as a new `login_anomaly_detected`
  audit event unconditionally, and gated behind an immediate TOTP
  step-up (`auth.sessions.requires_step_up`) when the account has TOTP
  confirmed. `AuthenticatedUser` refuses every route except
  `/auth/totp/step-up` and `/health/whoami` while the flag is set;
  `auth.record_step_up` clears it on a successful step-up.
  `auth.sessions.ip_address` is now actually populated (via
  `ConnectInfo`, direct-exposure topology) instead of always `NULL`.
  `/auth/login/finish` and `/health/whoami` responses both gained a
  `step_up_required` field.
- **Admin-configurable step-up policy.** `auth.auth_configuration.step_up_actions`
  is now actually read (via `auth::step_up_policy`) instead of sitting
  unused — gates "add a passkey to an account that already has one",
  previously hardcoded as unconditional. Seeded with `["add_passkey"]`
  so wiring this up doesn't silently disable existing protection. A new
  RLS policy lets any authenticated caller (not just admins) read
  `auth_configuration`, since an ordinary user needs to check whether
  their own action is gated.
- **TOTP re-enrollment no longer has a no-step-up-factor gap.**
  `auth.totp_credentials.pending_secret_encrypted` holds the
  re-enrollment candidate; the existing confirmed secret stays live
  until a code verifies against the pending one and gets promoted.
  Previously `/enroll/begin` overwrote the live secret immediately, so
  an abandoned re-enrollment left the account with no working step-up
  factor until it was finished.

### Removed
- **`POST /auth/totp/disable`** and the frontend's "Remove authenticator
  app" button. TOTP is step-up-only, never a login factor, so there was
  no security benefit to letting an account have zero step-up factor —
  only a self-inflicted-lockout risk. The account page now offers
  "Update authenticator app" (re-enrollment) instead, which replaces the
  factor rather than removing it with nothing to replace it.
- **`auth.auth_configuration.mandatory_passkey_enrollment`** — no code
  path ever made passkey enrollment optional; the column implied a
  control that didn't exist.

### Changed
- Session and ceremony cookies now carry `SameSite=Strict` (was `Lax`).
- `auth.create_session` gained a `p_requires_step_up` argument (no
  default — every caller now passes it explicitly).

### Documentation
- **[THREAT_MODEL.md](THREAT_MODEL.md)** — a formal threat/control
  matrix for the auth system: every threat considered, the control that
  closes it and where, and every deferred item or known gap named
  explicitly rather than left implicit.
- **[AUDIT_RETENTION.md](AUDIT_RETENTION.md)** — retention policy
  (indefinite by default, and structurally so — the audit table's
  append-only triggers block deletion outright) and a trigger-driven
  review process with runnable queries.

The backlog approved right after Phase II's close-out also shipped, found
via a real user bug report ("disable user feature is not available in the
FE") and the audit-log-viewer questions it raised: a standalone
disable-user action, the two audit-log gaps that were blocking a frontend
viewer, three new audit event types, the `onboarding_manager` role, and a
way to actually assign it.

### Added
- **`POST /auth/users/{id}/deactivate`** — admin-gated, wraps the
  already-built `auth.set_user_status` primitive in its own endpoint
  rather than only being reachable indirectly through account recovery.
  Refuses on self, on an already-deactivated target, and on a concurrent
  status change; writes a `user_deactivated` audit row with a real
  before/after status diff. `unitprep-ui`'s admin Users table gained a
  matching Disable button with a confirm step.
- **`GET /auth/audit-logs`** — admin-only listing over
  `auth.auth_audit_logs`, filterable by `event_type` and `user_id`
  (matches actor or target), keyset-paginated by `id`. No new `SECURITY
  DEFINER` function needed — `auth_audit_logs_select_admin_only` already
  grants exactly this access, unlike `list_users_for_admin`, which exists
  to bypass a *different* table's owner-only RLS for a cross-user join.
  Backs `unitprep-ui`'s new Audit Logs page: filters, keyset "load more",
  and a red/green before/after diff view for the events that carry one.
- **`audit_log::record()` now takes `ip_address` and a `Change`
  (before/after) pair.** Both columns existed in `auth.auth_audit_logs`
  since the very first migration with nothing ever writing them. Every
  existing call site was updated — `ip_address` is populated wherever
  `ConnectInfo` was already in scope or was cheap to add (login/
  registration success paths, invite creation/recovery, the new
  deactivate-user and role-change actions); the `/begin` legs and TOTP
  handlers still pass `None`, since neither has a natural IP source
  without disproportionate churn. `before_state`/`after_state` are
  populated for the schema's named diff-worthy events
  (`user_deactivated`, `account_recovery_initiated`, `role_changed`).
- **Three new audit event types.** `rate_limit_rejected` (fired from the
  auth/invite `GovernorLayer`'s error handler for the caller-driven
  rejection case only — the handler is synchronous with no `ConnectInfo`
  available, so this one carries no `ip_address`); `session_expired_access_attempt`
  (a session that genuinely existed and crossed its idle or absolute
  expiry, backed by a new `auth.check_session_expired` function —
  distinct from an ordinary missing/forged cookie, which still gets a
  plain 401 with no row); `authorization_failure` (an authenticated
  caller reaching an admin-gated action without the role for it).
- **`onboarding_manager`**, a second `auth.auth_role` enum value and
  `Role` variant — the second role named in the original architecture
  doc's extensible-role-column design. Every admin-gated `match
  admin.role` — unreachable while `Role` had one variant — gained an
  explicit arm that refuses it via a new shared `insufficient_role()` 403
  and writes an `authorization_failure` row.
- **Role selection.** `CreateInviteRequest` gained a `role` field
  (validated against `Role::from_db_text`, now public so a request-body
  validator and the session extractor share one parser) — any admin may
  assign either role at invite-creation time, and reissuing re-applies
  whatever role is submitted. `POST /auth/users/{id}/role` changes an
  already-enrolled user's role via a new `auth.set_user_role` `SECURITY
  DEFINER` function (mirroring `set_user_status` — `role` has no direct
  `UPDATE` grant), refuses a caller changing their own role, and writes a
  `role_changed` audit row with a before/after diff. `unitprep-ui` gained
  a role dropdown on the invite form and a per-row role dropdown on the
  admin Users table.

### Documentation
- **THREAT_MODEL.md** — new matrix rows for rate-limit abuse,
  session-expiry re-use, and role-based authorization failure; three
  `Known gaps` entries closed (disable-user, audit `ip_address`/before-
  after, rate-limit/session-expiry auditing); a new gap named
  (`onboarding_manager` has no permissions of its own yet — assigning it
  is solved, what it can do is still open).
- **AUDIT_RETENTION.md** — the trigger-driven immediate-review list
  gained `user_deactivated`, `role_changed`, and `authorization_failure`;
  practical queries extended to use `/auth/audit-logs` as the normal
  operational path, with raw SQL kept as the fallback.

Multi-role authorization: a user can now hold more than one role at
once, and every admin-gated endpoint checks a real permission instead of
matching on a hardcoded role name. `onboarding_manager`'s "no
permissions of its own yet" gap (named above) is closed as part of this.

### Added
- **Roles and permissions are now data**, not a single `auth.users.role`
  column: `auth.roles`, `auth.permissions`, `auth.role_permissions`, and
  `auth.user_roles` (many-to-many). Four system roles seeded (`admin`,
  `onboarding_manager`, `department_manager`, `sales`) with an initial
  8-key permission catalog matching each role's agreed capabilities.
  `auth.users.role` and the `auth_role` enum are dropped once nothing
  references them. RLS: the catalog tables are readable by any
  authenticated caller; `user_roles` is owner-or-admin read, admin-only
  write, with a `WITH CHECK` that structurally refuses a caller granting
  or revoking a role on their own account -- enforced by Postgres
  itself, not just the handler.
- **`AuthenticatedUser::require_permission`** replaces the `match
  admin.role { Role::Admin => {}, Role::OnboardingManager => {
  ...403... } }` block that had been duplicated across every admin-gated
  handler. Checks a permission key, records an `authorization_failure`
  audit row on refusal, and returns the existing shared 403. The closed
  `Role` enum is gone -- roles are open-ended now, so hardcoding them in
  Rust would defeat the point.
- **`POST /auth/users/{id}/roles`** (grant) and **`DELETE
  /auth/users/{id}/roles/{role_key}`** (revoke) replace the single-value
  `POST /auth/users/{id}/role` and the `auth.set_user_role` function it
  used -- plain RLS-scoped INSERT/DELETE on `auth.user_roles`, no
  `SECURITY DEFINER` function needed this time, since the table's own
  policies are the real enforcement. Revoking `admin` re-implements the
  last-remaining-admin guard against a role count instead of a role
  column. Both write `role_granted`/`role_revoked` audit rows carrying
  the target's full before/after role set, not just the one role that
  changed.
- **`GET /auth/roles`** -- the role/permission catalog, for the admin
  Roles page and any future role picker. No permission gate: any
  authenticated caller can already read this under RLS, and there's
  nothing sensitive in a role's name or its permission list.
- **`GET`/`PUT /auth/configuration`** -- org-wide auth policy, gated by
  a new `security_policies.manage` permission. Scoped to
  `step_up_actions` only: `allowed_factors` exists in the schema but no
  code path reads it, so a control for it would edit a value with no
  effect on real behaviour.
- Every user-creation path (invite issuance, the `bootstrap-admin` CLI)
  now grants a role as a second insert into `auth.user_roles` rather
  than a column value on the `INSERT INTO auth.users`.

### Fixed
- **`auth.resolve_session` was missing `permission_keys` entirely** --
  designed and coded against in `AuthenticatedUser`, never actually
  migrated into the database function. Every test exercised only the
  unauthenticated (no-cookie) path, so this shipped invisibly until a
  real login hit it. `resolve_session` now returns `permission_keys
  TEXT[]` alongside `role_keys`, resolved in the same query.

### Changed
- **`district_manager` renamed to `department_manager`** (role key and
  label), same day it was created -- "district manager" is already
  self-storage-industry terminology for a client-side facility manager,
  a different concept from this internal staff role.



## [1.5.0] - 2026-08-04

Phase 1 item 8: the admin Users listing that backs `unitprep-ui`'s new
Users tab.

### Added
- **`GET /auth/users`**, admin-only, read-only. Returns every non-deleted
  user's identity, role, status, and two facts the UI needs to decide
  what action makes sense for that row: `credential_count` (passkeys
  enrolled) and `totp_enrolled`.
- **`auth.list_users_for_admin()`**, a new `SECURITY DEFINER` function
  backing it. A plain admin-scoped query can't do this join itself:
  `auth.webauthn_credentials`'s RLS policy has no admin-bypass clause
  (unlike `auth.users`/`auth.totp_credentials`), so an ordinary admin
  query would see only its own credential rows and silently read every
  other user's `credential_count` as zero. This function checks the
  caller's role explicitly, the same way `auth.set_user_status` does,
  rather than widening the underlying RLS policy — which stays narrow on
  purpose, so an ordinary self-service credential read never accidentally
  becomes admin-browsable.
- Not audited — a listing is a read, and the audit trail records actions
  taken; every action this list's UI triggers (invite, reissue, recovery)
  already writes its own row via the existing `/auth/invites` endpoints.

## [1.4.0] - 2026-08-04

Phase 1 hardening is complete. Every product tool route now requires a
real session, closing the last gap between "auth exists" and "auth is
enforced" — and TOTP, which shipped last release as a login-fallback
factor, has been repurposed into step-up verification for sensitive
in-session actions instead, once admin-driven account recovery made the
gap it was plugging redundant. See `AUTHENTICATION.md` for the updated
architecture and roadmap.

### Added
- **Every tool route now requires `AuthenticatedUser`** — upload, discover,
  validate, correct, correct-group, exempt-dimensions, exclude-group(s),
  analyze, export, the group-file/unit-file selection and confirmation
  endpoints, and session cancellation. Previously these were reachable by
  anyone who could reach the API at all; a session is now the minimum bar
  for touching any of them, matching what already held for every `/auth/*`
  endpoint.
- **Sessions record their creator.** `owner_id` on a tool session is
  stamped from the caller's `AuthenticatedUser` at the two points a session
  is actually created (`/upload`, `/dedup/check`) rather than left `None`.
  Captured for attribution, not access control — nothing yet enforces "only
  the owner may act on their own session", since every authenticated caller
  in this v1 shares the one `admin` role and no product feature needs
  narrower scoping yet. The column exists so that when a real ownership
  model is needed (a usage/activity log, a multi-role future), the data
  already exists back to this release rather than needing a backfill.
- **`POST /auth/totp/step-up`.** Given a fresh code from a *confirmed*
  authenticator app credential, elevates the caller's own session
  (`auth.sessions.elevated_until`) for five minutes. Scoped to the one
  session that presented the code — proving a code on one browser must not
  silently elevate every other device the same user is signed in on
  elsewhere.
- **`/health/whoami` now reports `totp_enrolled`.** Lets a caller (in
  practice, the frontend) show enrollment status accurately instead of
  always presenting "enroll", which would risk walking an already-enrolled
  user into silently replacing their working credential — re-enrolling
  overwrites the secret immediately, with no warning at the point of
  writing it.
- Two new audit events: `totp_step_up_succeeded` and `totp_step_up_failed`,
  replacing TOTP's participation in `login_succeeded`/`login_failed` now
  that it no longer signs anyone in.

### Changed
- **Adding a passkey to an already-signed-in account now requires step-up.**
  `POST /auth/register/begin`'s authenticated branch (add-a-passkey-to-
  yourself) refuses with `403 step_up_required` unless the session is
  currently elevated. Planting a durable new credential is exactly the
  kind of sensitive, high-blast-radius action step-up exists to gate — a
  hijacked session cookie alone must no longer be sufficient for it. The
  unauthenticated invite path is unaffected: token possession is already
  its own authorization there.

### Removed
- **`POST /auth/login/totp` — TOTP can no longer log anyone in.** It
  shipped last release as a fallback for a device with no passkey
  enrolled, reasoning that stopped holding once admin-driven account
  recovery (also last release) started covering "lost your only passkey"
  through a human-verified path instead. Keeping a self-service login path
  through a static, phishable shared secret — fully capable of
  authenticating alongside a hardware-bound passkey — meant the account's
  real security floor was the weaker of the two factors, undercutting the
  whole point of going passkey-first. The verification primitive itself
  (`verify_code`, the lockout columns and functions) is unchanged and is
  now step-up's own foundation instead.

## [1.3.0] - 2026-08-03

Phase 2 identity/session work is complete: all eleven originally-planned
steps now exist (bootstrap-admin, registration, login, invitations,
sign-out, TOTP fallback, and the deactivation/soft-delete cascades that
retire every access path they leave behind). Phase 1 hardening has also
begun — the unauthenticated auth endpoints and invite creation are rate
limited, a misconfigured deployment can no longer silently serve session
cookies over plain HTTP, a deliberate audit-coverage sweep closed three
rejection paths that wrote no row while a comparable one did, and an
admin can now recover an account that has lost its only passkey without a
password reset ever existing to lose in the first place.

**What is NOT here yet**: no frontend at all — no login page, no invite
redemption, no route gating. Auth exists and is fully exercised by a
backend test harness; nothing outside the auth endpoints themselves
requires a session yet, so this is not an enforced product. See
`AUTHENTICATION.md` for the full architecture, audit posture, and the
remaining roadmap.

### Added
- **TOTP as a fallback factor** — `POST /auth/totp/enroll/begin`,
  `/enroll/confirm`, `/disable`, and `POST /auth/login/totp`. A fallback for a
  device with no passkey, **not** a second step stacked on one: a passkey is
  already multi-factor and phishing-resistant, and requiring both would add
  friction to every sign-in in exchange for the weaker property.
  Authenticator apps only, never SMS.

  Enrolment is two steps and the second is the point — a secret is stored
  with `confirmed_at` NULL and only counts once a real code verifies.
  Otherwise a user could believe they had a working fallback while having
  mis-scanned the secret, and would discover it at the moment they needed it
  and had no other way in.

  **Encryption at rest, the decision the schema deferred to this task:**
  ChaCha20-Poly1305 with a 32-byte key from `TOTP_ENCRYPTION_KEY`. This is
  the one credential in the schema that cannot be hashed — the server holds
  the whole secret and must reproduce it on every verification — which is why
  the column was named `secret_encrypted` before anything could write to it.
  The ciphertext is bound to its `user_id` through the AEAD's additional
  data, so a secret grafted onto another user's row fails to decrypt rather
  than working. A version byte prefixes the blob so key rotation is possible
  later without guessing which ciphertexts are which.

  Labelled honestly as an **app-level stopgap**: the key lives in the
  environment, so a dump *plus* the key is as good as plaintext. What it
  defends is the realistic case — a leaked backup, a shared database branch,
  a logged query result. Real KMS stays trigger-gated.

  Sign-in is rate-limited (five failures, then a 15-minute lock) because a
  six-digit code is guessable in a way a passkey assertion is not. The lock
  is time-bounded and applies only to the fallback, so it cannot be used to
  deny someone their account — the passkey path consults none of it. **If
  TOTP ever becomes primary or mandatory, that reasoning stops holding and
  the lockout needs revisiting.**
- **Sign-out and sign-out-everywhere** — `POST /auth/logout` and
  `POST /auth/logout/everywhere`. Sessions were previously unrevocable and
  simply accumulated. Revocation goes through two new `SECURITY DEFINER`
  functions because `app_service` holds no `UPDATE` on `auth.sessions` and
  must not: a column grant would permit writing `NULL` as readily as a
  timestamp, handing the application an *un-revoke* primitive and defeating
  the reason an opaque session token was chosen over a JWT. Both functions
  can only move `revoked_at` from `NULL` to `now()`, so a replayed sign-out
  cannot even shift the timestamp to obscure when the real one happened.

  Both take a **token hash rather than a user id**, which makes them
  self-authorizing — they can only act on the account whose live token the
  caller actually holds, so "sign this other user out of everything" is not
  a request that can be expressed. Sign-out-everywhere additionally requires
  the presented session to be currently valid, so a leaked expired cookie
  cannot be used to sign someone out of every device.

  Neither endpoint sits behind the authentication extractor, deliberately:
  signing out must succeed with a stale or missing cookie, or the one moment
  a user most needs the cookie gone is the moment it 401s.
- **Invitation creation**, `POST /auth/invites`, admin-only. Creates the
  account as `invited` and returns a one-time token, or reissues for an
  account that already exists and has not enrolled yet — retiring any
  outstanding invite first, so at most one link is ever live per account.
  Needs no new database objects: the existing `users_insert_admin_only` and
  `user_invites_admin_only` policies already permit it under an admin
  identity, so the database enforces admin-ness independently of the
  handler's own check. `user_invites.created_by` populates itself from the
  identity GUC, which is what that column default was written for.

  Refusals mirror `bootstrap-admin --reissue-invite` exactly — an account
  with a passkey enrolled, or one not in `invited` status, is declined with
  a reason. Unlike the unauthenticated endpoints these say *why*: the caller
  is an administrator who can already list users, so withholding it protects
  nothing.

  No `role` field is accepted. Only `admin` exists, so accepting one would
  add a client-controlled path to choosing a new account's privilege level
  for no capability gained.
- Audit events now record **`target_user_id`**, not just the actor. Invite
  creation is the first event where the two are different people, and they
  are passed as a named `Subjects` value rather than two adjacent
  `Option<Uuid>` parameters — a transposition there would misattribute an
  administrative action to the person it was performed on, compile cleanly,
  and look entirely normal in the row.

  **Consequence worth knowing:** because both audit foreign keys are
  `RESTRICT` and the table is append-only, an invited account becomes
  permanently un-hard-deletable the moment an invitation is issued for it.
  Previously that only happened once someone *did* something. A mistyped
  address therefore leaves a permanent row that can be soft-deleted but
  never removed.

- **Invitation acceptance.** An invited user enrols their first passkey by
  presenting the token from their invitation link to
  `POST /auth/register/begin`, and finishes signed in. Eligibility is
  enforced entirely inside a new `auth.resolve_invite_registration`
  SECURITY DEFINER lookup — the invite must be unused and unexpired, the
  user must still be `invited`, and they must hold zero credentials — so an
  anonymous caller can neither enumerate users nor enrol over an existing
  credential. The invite is consumed at `/finish`, after the credential
  verifies, in the same transaction that writes it: cancelling the
  authenticator prompt therefore costs nothing, leaving the user `invited`
  with a live invite and a retry that just works.

### Removed
- **`AUTH_BOOTSTRAP_ENABLED`, and the unauthenticated bootstrap enrolment
  path it gated.** Deleted rather than left unset — setting it now does
  nothing. It keyed first-passkey enrolment on an email address, so the
  endpoint was answerable by anyone who could guess one, with an
  environment variable as the only thing standing in the way. Possession of
  an unguessable invite token replaces it, which cannot be accidentally
  switched on by a misconfigured deployment.
- `auth.resolve_bootstrap_registration`, dropped in the same migration.
  Leaving it would have left a callable SECURITY DEFINER function matching
  any active user with no credentials by email alone, with its only guard
  removed.

  The first administrator is unaffected: `bootstrap-admin` already creates
  them as an `invited` user holding an invite, so they now walk the same
  enrolment route as everyone after them instead of a special case that
  runs once and is never exercised again.

### Fixed
- **Deactivating an account now revokes its sessions.** The deactivation
  trigger removed passkeys and TOTP secrets, and (as of the previous change)
  retired pending invites, but left live sessions alone — while being named
  after revoking every access path. Not exploitable, because
  `auth.resolve_session` already requires `status = 'active'` and
  `deleted_at IS NULL`, so a deactivated user's token resolved to nothing.
  What existed was a row that looked live and was not, which misleads anyone
  asking "who is signed in right now". Includes a backfill for accounts
  deactivated before this covered them.
- **The session cookie was not actually being cleared in a browser.**
  Clearing emitted a `Set-Cookie` with **no `Path`**, which per RFC 6265
  defaults to the requesting URI's directory rather than "everywhere" — so
  logging out at `/auth/logout` produced a deletion scoped to `/auth`, which
  never matched the real cookie's `Path=/`. Nothing was exposed (the session
  is revoked server-side) but the browser kept presenting a dead token, so
  every later request 401'd with a cookie attached.

  It survived because the existing test asserted "the cookie no longer reads
  back" against an in-memory jar, which models no path semantics at all and
  passes either way. Clearing now also *adds an expired cookie* rather than
  removing an entry, because removal is a no-op unless the cookie was parsed
  from the request — the case where a browser holds a cookie the server did
  not receive is exactly when telling it to drop one matters most. New tests
  assert the emitted header's attributes, which is the only part a browser
  consults.
- A refused passkey registration is now recorded. Previously a
  `403 registration_not_available` wrote no audit row and emitted no log
  line at all, while a failed *login* wrote a `login_failed` row -- so
  probing registration across a list of addresses was untraceable while
  the identical probing against login was recorded. That asymmetry was an
  oversight, not a policy. Refusals now write a `registration_failed`
  audit row naming the reason (`bootstrap_disabled`, `missing_email`,
  `not_eligible`) plus the attempted address, and log a `warn`. The HTTP
  response is unchanged and still byte-identical across every reason, so
  the endpoint remains useless for user enumeration -- what an attacker
  cannot distinguish and what an operator cannot see are separate
  properties, and only the first was ever intended.
- A registration whose credential fails verification also writes a
  `registration_failed` row (reason `credential_rejected`), matching
  login's existing `assertion_rejected`.
- Three more of the same asymmetry, found in a deliberate audit-coverage
  sweep rather than by accident: passkey `login_begin` wrote no row for an
  empty/whitespace email while the very next case (an address that fails
  to resolve to a credential) already logged `login_failed`; TOTP login's
  combined `email.is_empty() || !totp_configured()` check was one
  unaudited early return covering two distinct reasons, now split so each
  logs its own (`empty_email` / `totp_not_configured`); and an admin's
  attempt to re-invite an already-credentialed or wrong-status account
  produced a `tracing` line and nothing permanent. All three now write an
  audit row — the last one under a new `invite_refused` event, the
  refusal type carrying a structured reason rather than only a free-text
  message.

### Added
- Both halves of a WebAuthn ceremony now log a shared `correlation_id`,
  and it is recorded in the audit metadata of every ceremony outcome. Two
  concurrent ceremonies for the same user were previously
  indistinguishable in the log, since both lines carried only `user_id`.
  This is a *separate* id from the ceremony's own, deliberately: the
  ceremony id is the contents of the ceremony cookie, so logging that
  would put a live bearer value into ops output.
- `passkey_registered` audit metadata now records `device_bound` as
  reported by the authenticator at enrolment, rather than leaving it to be
  read off the credential row later.
- **Rate limiting on the unauthenticated auth endpoints and on invite
  creation.** The single biggest live gap flagged by both an internal
  review and external LLM review of AUTHENTICATION.md. `tower_governor`
  (an in-process token-bucket limiter over the `governor` crate) rather
  than a hosted or edge rate-limiting service, matching the
  library-over-service preference already established for the rest of
  auth. One shared bucket covers passkey register begin/finish, passkey
  login begin/finish, and TOTP login — deliberately one bucket for all
  five rather than one each, so a script cannot multiply its budget by
  spreading attempts across endpoints. Invite creation gets its own,
  more generous bucket, verified genuinely independent of the first with
  a real test rather than assumed. Keyed by real TCP peer address
  (`axum::serve`'s `ConnectInfo`), not a client-supplied header — there is
  no trusted-reverse-proxy policy yet, so behind a proxy that does not
  preserve the real peer this still limits correctly, just coarsely.
- **Admin-mediated account recovery**, `POST /auth/invites/recover`. An
  admin can now revoke every existing access path on an already-active
  account (passkeys, TOTP, live sessions, any outstanding invite) and
  issue a fresh invite in its place — the piece that makes "someone lost
  their only passkey" actually recoverable rather than only described.
  Deliberately its own endpoint, not a flag on invite creation: the two
  operations have very different blast radii if triggered by accident.
  Reuses the existing deactivation trigger by cycling the account's
  status through `deactivated` and back to `invited` inside one
  transaction, rather than writing a second copy of the credential/
  session/invite cleanup those migrations already implement. Backed by a
  new `auth.set_user_status` function — the first `SECURITY DEFINER`
  function in this schema whose safety depends on checking the caller's
  *role* rather than being scoped by a token or the caller's own id, since
  there is no such scoping available for "an admin changes someone else's
  status."

### Security
- `SESSION_COOKIE_SECURE=false` (the local-HTTP-dev escape hatch) could
  reach a real deployment silently — nothing checked it against
  `WEBAUTHN_RP_ORIGIN`. The server now refuses to start with that
  combination paired with a non-localhost origin, alongside the other
  fatal misconfiguration checks (database pool, WebAuthn backend).

## [1.2.0] - 2026-07-29

Passkey registration and sign-in work end to end. A user with a record in
`auth.users` can enrol a passkey and then authenticate with it, receiving a
session cookie later requests are verified against. Confirmed against a
real browser and real authenticators (Windows Hello, and independently
Proton Pass) talking to a real Postgres branch, not only in tests.

**What is NOT here yet**, since "auth works" would overstate it: no
sign-out, no invitation flow, no first-admin bootstrap beyond the env-gated
path below, no TOTP fallback, no admin UI. Enrolling the very first passkey
for an account still requires `AUTH_BOOTSTRAP_ENABLED`, which must stay
unset in any environment that matters. Five of the eleven planned
identity/session steps are done.

### Added
- Postgres connectivity via sqlx, connecting as a dedicated app_service
  role rather than the migration/owner role, so row-level security actually
  applies to application traffic. DATABASE_URL configures the connection
  pool, built lazily so a missing or incorrect credential does not block
  application startup. GET /health/db reports connectivity and confirms
  which role the pool is actually authenticating as.
- AuthBackend trait plus a webauthn-rs-backed implementation
  (WebauthnRsBackend), held in AppState behind Arc<dyn ...> the same way
  the existing session stores are, per the standing interface-first design
  rule.
- Session cookie plumbing: opaque token generation and hashing
  (session_token.rs) and httpOnly/Secure/SameSite cookie issuance, reading
  and clearing (session_cookie.rs). Deliberately unsigned and unencrypted --
  the cookie carries an opaque random token only ever trusted after a
  database round-trip, never decoded as a claim.
- AuthenticatedUser, an axum extractor resolving the session cookie into a
  verified user id and role via resolve_session(), plus
  begin_rls_transaction for handlers running further RLS-scoped queries
  under that identity. GET /health/whoami exercises the chain end to end.
- POST /auth/register/begin and /auth/register/finish. An authenticated
  caller enrols an additional passkey for themselves, taken from their
  session -- any email in the body is ignored, since honouring it would let
  a signed-in user write a credential onto another account. Otherwise the
  request falls to an env-gated bootstrap path, which exists only because
  nothing can sign a user in before a first credential exists.
- POST /auth/login/begin and /auth/login/finish. Verifying an assertion
  persists the credential state the ceremony advanced along with
  last_used_at; a frozen stored value would make the anti-cloning check
  pass indefinitely on authenticators that do implement a counter.
- Audit-event recording (auth/audit_log.rs) for login_succeeded,
  login_failed and passkey_registered, wired in from the start rather than
  switched on later so the record has no gap. Recording is deliberately
  infallible to callers: propagating a logging failure would let anyone who
  could break audit writes deny logins.
- SECURITY DEFINER lookups behind the unauthenticated paths
  (resolve_bootstrap_registration, resolve_login_candidate) enforcing
  eligibility in the database rather than in a handler, so a future
  endpoint that forgets to check cannot become a hole. Each answers every
  ineligible case identically, so neither can be used to discover which
  addresses have accounts.

### Security
- app_service held table-level UPDATE on auth.users, and
  users_update_own_or_admin is row-scoped rather than column-scoped, so a
  caller could have updated their own row with `SET role = 'admin'`. Not
  reachable today only because every existing user is already admin; it
  would have become live the moment a second role existed. UPDATE is now
  granted on first_name, last_name and job_title only -- role, status,
  company, email, deleted_at and deletion_reason are administrative and
  must go through a SECURITY DEFINER function that checks the caller.
- app_service likewise held UPDATE on auth.sessions, where the same
  row-scoped policy would have let a caller clear their own revoked_at --
  undoing "sign out everywhere" -- or extend expires_at indefinitely. Both
  defeat the reason an opaque token was chosen over a JWT: revocation that
  is instant and complete. The grant is revoked outright with no
  column-level replacement, since every sanctioned session mutation already
  runs through a SECURITY DEFINER function.
- scripts/setup_app_service_role.sql silently undid the auth.users fix.
  Its blanket `GRANT ... ON ALL TABLES IN SCHEMA auth` re-granted
  table-level UPDATE, so running a script documented as safe to re-run
  reopened the escalation vector with no error and no output. It now
  re-asserts the narrow grants.
- UPDATE and DELETE on auth_audit_logs are revoked from app_service,
  completing the append-only intent. A third barrier rather than a hole
  closed -- RLS default-deny and the append-only triggers already blocked
  both -- but it does not depend on policy evaluation being configured
  correctly, which is worth having on the one table whose whole value is
  being untamperable.

### Fixed
- The application could not talk to Neon's pooled endpoint at all. db.rs
  set `search_path` as a connection option, which travels in the Postgres
  startup packet and is rejected by the pooler ("unsupported startup
  parameter in options: search_path"), failing every query including
  /health/db. All application SQL is now schema-qualified and no
  search_path is set. Moving it to a per-connection SET would not have
  worked either: the pooler is transaction-mode, so a session-level SET is
  not reliably bound to the client that issued it and would have started
  leaking under concurrency. Not caught earlier because the unit tests use
  an unreachable lazy pool and execute no SQL, and because a direct
  connection accepts the parameter happily -- identical code worked or
  failed purely on which endpoint DATABASE_URL named.
- webauthn_credentials.device_bound is now written from the credential
  rather than left to the column's DEFAULT true, which had every row
  asserting the key could not leave its hardware. The first real passkey
  was backup-eligible -- a synced credential -- while its row said
  otherwise. No security decision reads the column, so nothing was
  bypassable; the value was simply false, and the planned admin
  enrolled-factor view would have shown it as fact.
- scripts/setup_app_service_role.sql could not bootstrap a fresh branch in
  any order: the role must exist before migrations run, because the RLS
  migrations end with GRANT EXECUTE to it, but its grants can only be
  applied after, since the schema and tables do not exist until then. Every
  schema- and table-dependent statement is now guarded, so the file is safe
  to run at any point -- run it, migrate, run it again.

### Changed
- Requiring device-bound (non-syncable) passkeys for accounts holding
  third-party credentials is dropped rather than deferred. Enforcing it
  would reject what Windows Hello and password managers produce by default,
  and break working from more than one machine, to protect secrets that do
  not exist yet. device_bound is recorded for visibility; nothing refuses a
  credential on it.
- The shared test pool's acquire_timeout is 50ms instead of sqlx's 30s
  default. The pool is lazy, so a handler path that unexpectedly reaches
  the database does not error -- it stalls for the full timeout and then
  errors, leaving the test passing and only the suite slower. Five login
  tests took 30.00s between them before this; they now take 0.05s and an
  unintended query fails fast instead of hiding.

## [1.1.5] - 2026-07-29

A fresh adversarial review pass (5 parallel reviewers, one per crate/
layer boundary) after 1.1.4 shipped, run to close out the refactor
before a code-quality conclusion. No new functionality.

### Fixed
- `Session::complete_discovery` didn't bump `data_generation`, reopening
  the exact class of race 1.1.3/1.1.4 closed for corrections/exemptions/
  exclusions: handlers reachable after `Analyzed`/`Exported` (unit-file
  format resolution, group-file selection) mutate `SessionData` directly
  without going through a generation-bumping method, so a change landing
  in `/analyze` or `/export`'s read -> write-back gap could still have
  its safety-net stage downgrade silently re-promoted. Fixed at the
  single funnel (`complete_discovery`) rather than each handler.

### Changed
- Moved `find_typo_variant_candidates` from `report.rs` into
  `similarity.rs`, matching this crate's own documented one-module-
  per-signal convention (next to `relatedness.rs`'s equivalent).
- Corrected `dedup/RULES.md`'s "individually well-formed email" wording
  to match what `all_emails_present_and_distinct` actually checks
  (non-blank and mutually distinct -- no format validation).
- Removed two no-op entries from `STREET_SUFFIXES`.

### Added
- A regression test for the `complete_discovery` generation-bump fix.
- Completeness tests for `FIELD_SPECS`/`CATEGORY_PRIORITY` against every
  `FieldName`/`FieldCategory` variant (compile-error-on-drift, via an
  exhaustive match with no wildcard arm).
- Tests exercising `GroupCheckAcknowledgments` with real (non-default)
  values at the `unit-group` crate's own unit-test level.
- A zero-row dedup pipeline test.

## [1.1.4] - 2026-07-28

Closes out the two file splits and two concurrency gaps this pass's own
prior audits had deferred, plus the remaining flagged test-coverage
gaps and a dead code-path removal. No new functionality.

### Fixed
- Session-cleanup sweep held the entire session map's write lock for
  its full O(n) scan, blocking every concurrent `save`/`get_handle`/
  `delete` call for the whole sweep, not just the sessions actually
  being removed. Now scans for expired candidates under a read lock,
  then takes the write lock only to remove them, re-verifying each is
  still expired immediately before removal.
- `cancel_session`'s concurrent-mutation race (previously only logged,
  not fixed, in 1.1.3): a concurrent handler already holding its own
  handle to a session could still complete a mutation on it after
  `cancel_session` removed it from the map, with no way for any future
  caller to ever observe that write. Fixed with a `cancelled` flag set
  under the session's own write lock before removal; every generic
  session-access method now treats a cancelled session exactly like a
  nonexistent one, mirroring the existing owner-mismatch gate -- no
  individual handler needed changes.
- Removed the `acknowledge_errors` export override -- dead code with
  no reachable UI path (the frontend's own "Continue" button is
  disabled until every issue, not just Errors, is already resolved, so
  the override could never fire). Every real `Severity::Error` issue
  type already has inline correction UI; the one condition that stays
  unconditionally blocking, a file that failed to parse, correctly
  should.

### Changed
- Split `api/validate.rs`'s summary-building logic into
  `validate/summary.rs` (285 -> 211 lines).
- Split `discover/compute.rs`'s selection logic into
  `discover/selection.rs` (335 -> 176 lines).

### Added
- Regression tests for both concurrency fixes above, a Unicode/
  diacritic name-matching test, two smallest-input pipeline tests (1
  and 2 fabricated records), an error-shape sweep test across several
  endpoints, and an oversized-request-body test.

## [1.1.3] - 2026-07-28

12 real bugs found via a 6-agent adversarial review (core parsers, dedup,
unit-group, and the HTTP/session layer), each confirmed empirically
before fixing and each with its own regression test; no new
functionality.

### Fixed
- SpreadsheetML `<![CDATA[...]]>` cell values were silently dropped
  (no parser match arm for that event) instead of surfacing an error.
- Excel float cells silently saturated to `i64::MAX`/`MIN` for a
  whole-number value outside `i64`'s range instead of erroring.
- A blank phone-number prefix on one dedup record falsely flagged a
  Phone-category mismatch even when the actual phone number matched.
- `group_key` didn't collapse internal whitespace, so two records
  differing only by a double space landed in separate tenant groups.
- The typo-variant candidate sort used `partial_cmp(...).unwrap()`,
  a latent NaN panic path; switched to `total_cmp`.
- Dimension exemption was silently ineffective when a unit's UnitGroup
  name was itself a malformed dimension attempt (e.g. `"10x"`).
- Unit-number identifiers weren't trimmed consistently (asymmetric
  with UnitGroup), across validation, corrections, and `/correct-group`.
- Comma-decimal dimension values (`"10,5"`) were rejected as invalid.
- Repeating an identical `/correct-group` rename request returned 400
  the second time instead of succeeding as a no-op.
- A concurrent correction/exemption/exclusion landing between
  `/analyze` or `/export`'s read and delayed write-back could have its
  safety-net stage downgrade silently undone, re-promoting the
  workflow using stale pre-correction data -- confirmed live. Fixed
  with a session data-generation counter checked before each write-back.
- Malformed JSON, a wrong Content-Type, or an oversized body rejected
  with a plain-text response instead of this API's standard
  `{error, message}` shape.
- `/correct` and `/exempt-dimensions` accepted a `unit_number` that
  didn't exist in the file, or was ambiguous (shared by 2+ rows from
  an already-flagged duplicate), silently storing a dead or
  data-corrupting entry with no error.

## [1.1.2] - 2026-07-28

Test coverage and two crash fixes found through it; no new functionality.

### Fixed
- `cell_to_string` (Excel parsing): a date-typed cell with an extreme
  serial number could panic inside chrono's `TimeDelta` construction,
  not just return `None` as calamine's own doc comment claims. Wrapped
  in `catch_unwind`, falling back to the raw serial number for that one
  cell instead of failing the whole request.
- SpreadsheetML parsing: `ss:Index`/`ss:MergeAcross` were parsed from
  untrusted XML into `usize` with no bound, then fed straight to
  `Vec::resize` -- a single crafted cell (e.g. `ss:Index="99999999999999"`)
  could attempt an astronomical allocation and abort the whole process,
  not a `panic!` the catch-panic middleware could intercept. Both
  attributes are now clamped to `1..=16384` (Excel's own real column
  limit) at the point they're parsed.

### Added
- Property-based/fuzz tests (via `proptest`) for all three file
  parsers -- the two fixes above were both found this way.
- Real HTTP-level integration tests (`src/api/http_integration_tests.rs`):
  bind the actual router to a loopback port and drive it with a real
  `reqwest` client, including an automated regression test for the CORS
  credentials fix (previously verified only by hand in a browser).
- Regression tests for the analyze/export session write-back race.
- `cargo-llvm-cov` wired in as the primary coverage tool (`cargo cov`
  / `cargo cov-summary` aliases in `.cargo/config.toml`) -- current
  baseline 84% lines workspace-wide. `cargo-tarpaulin` also available
  (`cargo cov-tarpaulin`) as an occasional independent cross-check, not
  a second tool to run routinely alongside llvm-cov.

## [1.1.1] - 2026-07-28

No new functionality; a full post-1.1.0 hygiene, security, and
correctness pass across every crate.

### Added
- `SessionMetadata.owner_id: Option<Uuid>` plus owner-gated
  `with_owned_session`/`with_owned_session_mut` store lookups, threaded
  through both session-creating handlers (currently passing `None` --
  no `AuthenticatedUser` exists on either yet).
- Router-wide panic-catching middleware
  (`tower_http::catch_panic::CatchPanicLayer`) returning the project's
  own `ApiErrorBody` 500 shape instead of dropping the connection.
- First test coverage for the upload handler (4 tests via a real
  multipart request).
- A synthetic full-pipeline dedup test covering grouping, flagging,
  typo-variant detection, and relatedness together on fabricated data.
- `.cargo/audit.toml` documenting one accepted, non-reachable
  `cargo-audit` finding (`RUSTSEC-2023-0071`, an optional `sqlx-mysql`
  dependency never compiled into this binary).

### Changed
- Applied `cargo fmt` across the entire workspace (no rustfmt.toml
  existed before this; formatting was never mechanized).
- `unit-group`: removed unnecessary clones in `analyze_batch`, added a
  group-fingerprint cache to `RowScan`, indexed `apply_corrections`
  lookups by unit instead of rescanning per row, removed dead/lossy
  code in `models.rs` and consolidated on a single public type name for
  advisory issues.
- `dedup`: fixed 4 blank-vs-normalized-value comparison/display bugs
  across `comparison.rs`/`phrasing.rs`, an address-join bug in
  `relatedness.rs` that could collapse two different addresses into
  one string, and a title-casing bug for names like `O'Brien`.
- `csv_export.rs` and the dedup CSV/XLSX writers now share single
  helpers (`write_csv`, `record_field_values`) instead of each
  independently repeating the same boilerplate/field list.
- `DiscoverResponse::from(&DiscoveryResult)` replaces ~60 lines of
  hand-copying with ~15; `Session::effective_documents_for(names)`
  lets `analyze`/`validate`/discovery filter to relevant documents
  before the mapping/correction/exclusion transform instead of after;
  `AnalysisResults` is now `Arc`-wrapped so passing it around a session
  is a refcount bump instead of a deep clone.
- Bumped `quick-xml` (0.36 -> 0.41, direct and via `calamine`) and
  `calamine` (0.25 -> 0.36) for two RustSec advisories reachable via
  uploaded SpreadsheetML files.

### Fixed
- A real CORS gap: the frontend's shared fetch hooks send
  `credentials: "include"`, but the API's `CorsLayer` didn't set
  `Access-Control-Allow-Credentials: true` -- which per the Fetch/CORS
  spec makes a credentialed response invisible to the browser
  regardless of whether a cookie exists yet. Verified live against a
  running frontend, not just by reading code.

## [1.1.0] - 2026-07-20

### Added
- Duplicate tenant check — a second, independent tool: `unitprep-dedup`
  (new workspace crate — grouping/comparison/typo-variant domain logic,
  depending only on `unitprep-core`, no session/HTTP/export concerns)
  plus its own session type and three endpoints, `POST /dedup/check`,
  `POST /dedup/report`, `POST /dedup/export`. Every typo/name-variant
  candidate is surfaced for human confirmation, never auto-merged.
  Domain logic verified against real facility exports, byte-for-byte
  matching an independently-confirmed reference-script run on one of
  them.
- CSV parsing now tolerates a trailing unnamed column beyond the
  header's last field (a real, consistent quirk in some facility
  export tools) instead of rejecting every row of an affected file.
- Startup log now includes the process's PID, so a specific running
  instance can be identified from its own log output without a
  separate `ps`/`ss` lookup.

### Changed
- UnitGroup's own domain logic (discovery-result/validation-result
  data, batch building, fingerprint matching, validation rules,
  correction overlays) moved out of the binary's `src/domain/` into the
  previously-empty `unitprep-unit-group` crate — the same
  domain/session boundary `unitprep-dedup` established, applied back to
  the original tool. `Session`/`WorkflowStage`/`StageError` (the stage
  machine) stay in the binary, in `src/application/unit_group_session.rs`.
  No behavior change — verified via the full existing test suite (moved
  intact, none lost) and a live run of the full
  upload/discover/validate/analyze/export pipeline.
- Calling an endpoint before the session has reached the required
  workflow stage (e.g. `/analyze` before `/validate`) now returns
  `409 Conflict` with a structured `{ error, message }` body, instead of
  a fake all-zero `200` success that looked identical to a real,
  successful "nothing to report" result. Every error response across
  the API now shares this same `{ error, message }` shape.
- `POST /group-file/select` now returns the same structured error shape
  as the rest of the API instead of a `200` with `{ success: false }`:
  `409 Conflict` if called before discovery has completed, `400 Bad
  Request` (`group_file_invalid`) if the named file wasn't one
  discovery actually found.
- `POST /session/cancel` stays intentionally idempotent (always `200`,
  even for an unknown session id — that's not an error worth surfacing)
  but its response now includes `deleted: bool`, so a caller that does
  care can tell "deleted a real session" apart from "there was nothing
  there," without changing the success contract.

### Fixed
- `/discover` no longer gets permanently stuck when zero master group
  files are found — the exact shape of a net-new client with nothing
  in QMS yet to cross-reference against. `ready` previously required
  `group_files.len() == 1`, so zero candidates was treated the same as
  "ambiguous, needs selection," except with no candidates to select
  from — a real dead end with no way to proceed. Analysis already
  handled a missing reference set correctly (every discovered group
  becomes net-new); only the discovery-readiness gate was wrong. Zero
  or one candidate is now ready; only *more than one* still requires
  `/group-file/select`. `DiscoverResponse` also now includes
  `discovered_group_names` — the distinct UnitGroup values found across
  the discovered unit files (reusing `build_batch_from_documents`) — so
  the UI can show the user what was actually found before they commit
  to validate/export, most useful exactly when there's no master file
  to cross-check against yet.
- Starting a second instance against an already-bound port used to
  panic with a bare "Address already in use" and no next step. It now
  exits cleanly with a message pointing at the command to find the
  other process (`ss -ltnp | grep :PORT` or `lsof -i :PORT`) — the
  actually useful fact (which *other* process holds the port) isn't
  something this process can look up about itself, so the fix points at
  how to find it rather than guessing at a PID.

## [1.0.0] - 2026-07-08

### Added
- Validation issues now report the specific affected unit ids and a
  human-readable detail string, not just a count.
- `POST /correct` — applies a single corrected value to a flagged unit
  (e.g. Width) as a session-level overlay and immediately re-validates,
  without needing a full re-upload.
- `POST /exempt-dimensions` — marks a catalog entry that legitimately
  isn't a dimensioned unit (an office, an owner's apartment, etc.) as
  exempt from the "Invalid dimensions" check, instead of requiring a
  fabricated Width/Length.
- `POST /export` accepts `acknowledge_errors` — an explicit human
  override to export despite unresolved validation errors, logged when
  used. Never applied silently.
- Real parsing support for Excel 2003 SpreadsheetML XML, content-sniffed
  regardless of file extension (some facility export tools mislabel this
  format with a `.xls` extension).
- Every session-scoped endpoint now returns a distinct
  `404 Session not found or expired` instead of silently faking a
  zero-value success response.
- `HOST`/`PORT` env vars for the bind address; defaults to `0.0.0.0`
  instead of `127.0.0.1` so the app is reachable from outside a
  container by default.
- `CORS_ALLOWED_ORIGINS` env var to configure allowed origins beyond the
  local dev defaults.
- `version` field on `GET /health`, read from `CARGO_PKG_VERSION`.
- Endpoint-level test coverage (`src/api/*.rs`) for every new endpoint
  and the session-not-found behavior, alongside the existing domain-level
  unit tests.

### Changed
- "Invalid dimensions or area values" simplified to "Invalid
  dimensions" — Area is no longer validated or offered as a correctable
  field.
- Default logging verbosity reduced from per-file `DEBUG` noise to
  aggregate `INFO` summaries per pipeline stage; `RUST_LOG` now actually
  controls the level instead of being force-overridden to `debug`.

### Removed
- The "Area does not match width × length" validation check — Area is a
  derived value (Width × Length), not an independent fact worth
  validating or correcting on its own.

[Unreleased]: https://github.com/quikstorboris/unitprep-api/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/quikstorboris/unitprep-api/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/quikstorboris/unitprep-api/releases/tag/v1.0.0
