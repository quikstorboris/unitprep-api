//! G1: a secret-bearing input type must never print its secret through
//! `Debug`, so a future `tracing::..!(request = ?request)` cannot leak it.

use crate::api::clickup_connection::SaveTokenRequest;
use crate::api::dropbox_settings::UpdateDropboxSettingsRequest;
use crate::api::process_street_settings::UpdateProcessStreetSettingsRequest;

const SECRET: &str = "TOPSECRET-do-not-log";

#[test]
fn the_dropbox_settings_request_redacts_its_secrets_but_keeps_the_rest() {
    let request = UpdateDropboxSettingsRequest {
        app_key: "visible-app-key".to_string(),
        app_secret: SECRET.to_string(),
        refresh_token: SECRET.to_string(),
        root_namespace_id: "ns-1".to_string(),
        root_path: "/QMS Onboarding".to_string(),
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains(SECRET), "{printed}");
    assert!(printed.contains("<redacted>"));
    assert!(printed.contains("visible-app-key") && printed.contains("/QMS Onboarding"));
}

#[test]
fn the_process_street_settings_request_redacts_the_api_key() {
    let request = UpdateProcessStreetSettingsRequest {
        schedule_mode: "interval".to_string(),
        sync_interval_hours: 6,
        sync_time: None,
        sync_timezone: None,
        api_key: SECRET.to_string(),
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains(SECRET), "{printed}");
    assert!(printed.contains("interval") && printed.contains("<redacted>"));
}

#[test]
fn the_clickup_token_request_redacts_the_token() {
    let printed = format!(
        "{:?}",
        SaveTokenRequest {
            token: SECRET.to_string()
        }
    );
    assert!(!printed.contains(SECRET), "{printed}");
}
