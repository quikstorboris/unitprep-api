//! Closes THREAT_MODEL.md's "no formal, automated check that every new
//! privilege-gated handler actually calls `require_permission`" gap. See
//! `route_access`'s module doc for the compile-time half (every route
//! must declare a `RouteAccess`); this is the runtime half -- for every
//! route `build()` classified as `RouteAccess::Permission`, prove the
//! exact handler wired up to it actually 403s a caller holding no
//! permissions, the same `empty_state()`/dummy-argument pattern this
//! codebase's existing per-handler tests already use (see e.g.
//! `clients_companies::tests::archiving_refuses_insufficient_permission_without_touching_anything`).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use axum::extract::{ConnectInfo, Path, Query};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::Json;
use uuid::Uuid;

use crate::api::clients_companies;
use crate::api::route_access::RouteAccess;
use crate::api::test_support::{empty_state, test_user};
use crate::api::{
    auth_audit_logs, auth_audit_logs_export, auth_configuration, auth_invites, auth_user_role,
    auth_user_status, auth_users, client_ops_activity_logs, client_ops_activity_logs_export,
    client_ops_qms_tags, clients_create, clients_elavon, clients_manual_link, clients_resync,
    clients_sync, dropbox_settings, process_street_settings, tool_runs,
};

use super::routes::build;

type BoxFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

/// `(path, method, assert_denied)` -- one entry per `Permission`-
/// classified route, as returned by `permission_route_checks` below.
type PermissionRouteCheck = (&'static str, Method, fn() -> BoxFuture);

fn local_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

/// One entry per `RouteAccess::Permission` route `build()` registers
/// below -- calls the exact handler wired up to that (path, method),
/// with `test_user()` (a valid session holding zero permissions).
/// Adding a `Permission`-classified `gated_route` without a matching
/// entry here fails `every_permission_route_is_covered_and_enforced`
/// below by construction: the whole point of this list is to make
/// that omission loud instead of silent.
fn permission_route_checks() -> Vec<PermissionRouteCheck> {
    vec![
        (
            "/auth/invites",
            Method::POST,
            (|| {
                Box::pin(auth_invites::create_invite(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Json(auth_invites::CreateInviteRequest {
                        email: "ada@example.com".to_string(),
                        first_name: "Ada".to_string(),
                        last_name: "Lovelace".to_string(),
                        company: "quikstor".to_string(),
                        job_title: None,
                        role: "onboarding_manager".to_string(),
                    }),
                ))
            }) as fn() -> BoxFuture,
        ),
        ("/auth/invites/recover", Method::POST, || {
            Box::pin(auth_invites::recover_account(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Json(auth_invites::RecoverAccountRequest {
                    email: "someone@example.com".to_string(),
                }),
            ))
        }),
        ("/auth/users", Method::GET, || {
            Box::pin(auth_users::list_users(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/auth/users/export", Method::GET, || {
            Box::pin(auth_users::export_users(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/auth/users/{id}/deactivate", Method::POST, || {
            Box::pin(auth_user_status::deactivate_user(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
            ))
        }),
        ("/auth/users/{id}/reactivate", Method::POST, || {
            Box::pin(auth_user_status::reactivate_user(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
            ))
        }),
        ("/auth/users/{id}/roles", Method::POST, || {
            Box::pin(auth_user_role::grant_role(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
                Json(auth_user_role::GrantRoleRequest {
                    role: "onboarding_manager".to_string(),
                }),
            ))
        }),
        ("/auth/users/{id}/roles/{role_key}", Method::DELETE, || {
            Box::pin(auth_user_role::revoke_role(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Path((Uuid::new_v4(), "onboarding_manager".to_string())),
            ))
        }),
        ("/auth/configuration", Method::GET, || {
            Box::pin(auth_configuration::get_configuration(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/auth/configuration", Method::PUT, || {
            Box::pin(auth_configuration::update_configuration(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Json(auth_configuration::UpdateAuthConfigurationRequest {
                    step_up_actions: vec![],
                }),
            ))
        }),
        ("/client-ops/qms-tags", Method::POST, || {
            Box::pin(client_ops_qms_tags::create_qms_tag(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Json(client_ops_qms_tags::CreateQmsTagRequest {
                    tag_key: "e.test".to_string(),
                    label: "Test".to_string(),
                    category: "Tenant".to_string(),
                }),
            ))
        }),
        ("/client-ops/qms-tags/{tag_key}", Method::PUT, || {
            Box::pin(client_ops_qms_tags::update_qms_tag(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Path("e.test".to_string()),
                Json(client_ops_qms_tags::UpdateQmsTagRequest {
                    label: "Test".to_string(),
                    category: "Tenant".to_string(),
                }),
            ))
        }),
        (
            "/client-ops/qms-tags/{tag_key}/deactivate",
            Method::PATCH,
            || {
                Box::pin(client_ops_qms_tags::deactivate_qms_tag(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path("e.test".to_string()),
                ))
            },
        ),
        (
            "/client-ops/qms-tags/{tag_key}/reactivate",
            Method::PATCH,
            || {
                Box::pin(client_ops_qms_tags::reactivate_qms_tag(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path("e.test".to_string()),
                ))
            },
        ),
        ("/client-ops/activity-logs", Method::GET, || {
            Box::pin(client_ops_activity_logs::list_activity_logs(
                axum::extract::State(empty_state()),
                test_user(),
                Query(client_ops_activity_logs::ActivityLogQuery {
                    limit: None,
                    before_id: None,
                    event_type: None,
                    entity_type: None,
                    actor_user_id: None,
                }),
            ))
        }),
        ("/client-ops/activity-logs/event-types", Method::GET, || {
            Box::pin(client_ops_activity_logs::list_event_types(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/client-ops/activity-logs/export", Method::POST, || {
            Box::pin(client_ops_activity_logs_export::export_activity_logs(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Json(client_ops_activity_logs_export::ExportActivityLogsRequest {
                    date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                    date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                    event_types: vec![],
                    entity_types: vec![],
                    actor_user_ids: vec![],
                }),
            ))
        }),
        (
            "/client-ops/activity-logs/export/preview",
            Method::POST,
            || {
                Box::pin(client_ops_activity_logs_export::preview_activity_logs(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    Json(client_ops_activity_logs_export::ExportActivityLogsRequest {
                        date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                        date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                        event_types: vec![],
                        entity_types: vec![],
                        actor_user_ids: vec![],
                    }),
                ))
            },
        ),
        ("/clients", Method::POST, || {
            Box::pin(clients_create::create_client(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Json(clients_create::CreateClientRequest {
                    company_intake_run_id: "abc123".to_string(),
                    company: Default::default(),
                    facilities: vec![],
                }),
            ))
        }),
        ("/clients/{company_id}/archive", Method::POST, || {
            Box::pin(clients_companies::archive_company(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
            ))
        }),
        ("/clients/{company_id}/unarchive", Method::POST, || {
            Box::pin(clients_companies::unarchive_company(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
            ))
        }),
        ("/clients/{company_id}/resync/preview", Method::POST, || {
            Box::pin(clients_resync::preview_resync(
                axum::extract::State(empty_state()),
                test_user(),
                Path(Uuid::new_v4()),
            ))
        }),
        ("/clients/{company_id}/resync/apply", Method::POST, || {
            Box::pin(clients_resync::apply_resync(
                axum::extract::State(empty_state()),
                test_user(),
                Path(Uuid::new_v4()),
                Json(clients_resync::ApplyResyncRequest {
                    resolutions: vec![],
                }),
            ))
        }),
        ("/clients/{company_id}/manual-link", Method::POST, || {
            Box::pin(clients_manual_link::manual_link(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
                Json(clients_manual_link::ManualLinkRequest {
                    facility_id: Uuid::new_v4(),
                    workflow: clients_manual_link::ManualLinkWorkflow::Intake,
                    run_id: "abc123".to_string(),
                }),
            ))
        }),
        ("/clients/{company_id}", Method::DELETE, || {
            Box::pin(clients_companies::delete_company(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
            ))
        }),
        (
            "/clients/{company_id}/facilities/{facility_id}/elavon/link",
            Method::POST,
            || {
                Box::pin(clients_elavon::link_facility_elavon(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path((Uuid::new_v4(), Uuid::new_v4())),
                    Json(clients_elavon::LinkElavonRequest {
                        merchant_account_run_id: "abc123".to_string(),
                    }),
                ))
            },
        ),
        (
            "/clients/{company_id}/facilities/{facility_id}/elavon/link",
            Method::DELETE,
            || {
                Box::pin(clients_elavon::unlink_facility_elavon(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path((Uuid::new_v4(), Uuid::new_v4())),
                ))
            },
        ),
        (
            "/clients/{company_id}/facilities/{facility_id}/elavon/resync",
            Method::POST,
            || {
                Box::pin(clients_elavon::resync_elavon_data(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path((Uuid::new_v4(), Uuid::new_v4())),
                ))
            },
        ),
        (
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}",
            Method::DELETE,
            || {
                Box::pin(tool_runs::delete_tool_run(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
                ))
            },
        ),
        ("/clients/sync", Method::POST, || {
            Box::pin(clients_sync::start_sync(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Query(clients_sync::StartSyncQuery { force: false }),
            ))
        }),
        ("/integrations/process-street/settings", Method::GET, || {
            Box::pin(process_street_settings::get_settings(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/integrations/process-street/settings", Method::PUT, || {
            Box::pin(process_street_settings::update_settings(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Json(
                    process_street_settings::UpdateProcessStreetSettingsRequest {
                        schedule_mode: "interval".to_string(),
                        sync_interval_hours: 24,
                        sync_time: None,
                        sync_timezone: None,
                        api_key: "test".to_string(),
                    },
                ),
            ))
        }),
        ("/integrations/dropbox/settings", Method::GET, || {
            Box::pin(dropbox_settings::get_settings(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/integrations/dropbox/settings", Method::PUT, || {
            Box::pin(dropbox_settings::update_settings(
                axum::extract::State(empty_state()),
                test_user(),
                HeaderMap::new(),
                Json(dropbox_settings::UpdateDropboxSettingsRequest {
                    app_key: "key".to_string(),
                    app_secret: "secret".to_string(),
                    refresh_token: "token".to_string(),
                    root_namespace_id: "ns".to_string(),
                    root_path: "/".to_string(),
                }),
            ))
        }),
        ("/auth/audit-logs", Method::GET, || {
            Box::pin(auth_audit_logs::list_audit_logs(
                axum::extract::State(empty_state()),
                test_user(),
                Query(auth_audit_logs::AuditLogQuery {
                    limit: None,
                    before_id: None,
                    event_type: None,
                    user_id: None,
                }),
            ))
        }),
        ("/auth/audit-logs/event-types", Method::GET, || {
            Box::pin(auth_audit_logs::list_event_types(
                axum::extract::State(empty_state()),
                test_user(),
            ))
        }),
        ("/auth/audit-logs/export", Method::POST, || {
            Box::pin(auth_audit_logs_export::export_audit_logs(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                HeaderMap::new(),
                Json(auth_audit_logs_export::ExportAuditLogsRequest {
                    date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                    date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                    event_types: vec![],
                    user_ids: vec![],
                    ip_address: None,
                }),
            ))
        }),
        ("/auth/audit-logs/export/preview", Method::POST, || {
            Box::pin(auth_audit_logs_export::preview_audit_logs(
                axum::extract::State(empty_state()),
                test_user(),
                local_addr(),
                Json(auth_audit_logs_export::ExportAuditLogsRequest {
                    date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                    date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                    event_types: vec![],
                    user_ids: vec![],
                    ip_address: None,
                }),
            ))
        }),
    ]
}

#[tokio::test]
async fn every_permission_route_is_covered_and_enforced() {
    let (_, manifest) = build(empty_state()).into_parts();
    let checks = permission_route_checks();

    let permission_entries: Vec<_> = manifest
        .iter()
        .filter(|entry| matches!(entry.access, RouteAccess::Permission { .. }))
        .collect();

    assert!(
        !permission_entries.is_empty(),
        "expected at least one RouteAccess::Permission route -- did classification regress?"
    );

    for entry in permission_entries {
        let check = checks
            .iter()
            .find(|(path, method, _)| *path == entry.path && *method == entry.method);

        let Some((_, _, assert_denied)) = check else {
            panic!(
                "{} {} is classified as RouteAccess::Permission but has no entry in \
                 permission_route_checks() -- add one so this test can prove the handler \
                 actually enforces it",
                entry.method, entry.path
            );
        };

        let response = assert_denied().await;

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{} {} is classified as requiring a permission, but a caller with none got {} \
             instead of 403 -- the handler may never call require_permission",
            entry.method,
            entry.path,
            response.status()
        );
    }
}

/// The inverse of the check above: every entry in
/// `permission_route_checks()` must actually correspond to a real
/// `Permission`-classified route in the manifest -- otherwise a
/// stale entry (route renamed/removed) would silently stop proving
/// anything.
#[tokio::test]
async fn every_permission_route_check_matches_a_real_manifest_entry() {
    let (_, manifest) = build(empty_state()).into_parts();

    for (path, method, _) in permission_route_checks() {
        let found = manifest.iter().any(|entry| {
            entry.path == path
                && entry.method == method
                && matches!(entry.access, RouteAccess::Permission { .. })
        });

        assert!(
            found,
            "permission_route_checks() has an entry for {method} {path}, but build()'s \
             manifest has no matching RouteAccess::Permission route -- remove the stale \
             check or fix the classification"
        );
    }
}
