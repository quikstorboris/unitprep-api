//! The QMS template-tag reference catalog (`client_ops.qms_tag`) --
//! Phase 1 of the QMS Template Tagging Assistant. A hand-maintained
//! stand-in for QMS one day exposing its own tag list via its own API;
//! see the vault's "QMS Template Tags" notes for why this exists and
//! what it is seeded with.
//!
//! Read is open to any authenticated caller (`qms_tag_select_authenticated`
//! -- catalog data, nothing sensitive). Every mutation requires the
//! `client_ops.manage_tags` permission, held by `admin`,
//! `onboarding_manager`, and `department_manager` alike -- Boris's call:
//! this is a band-aid for a gap in QMS's own API, not a client operation
//! in its own right, so it does not follow `client_ops.perform`'s usual
//! admin-excluded shape. Every mutation also writes a
//! `client_ops::audit_log` row -- a distinct, non-security trail, per the
//! same conversation.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log::{self, Change};

const PERMISSION: &str = "client_ops.manage_tags";
const ENTITY_TYPE: &str = "qms_tag";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct QmsTag {
    pub tag_key: String,
    pub label: String,
    pub category: String,
    pub is_active: bool,
}

#[derive(Debug, Serialize)]
pub struct ListQmsTagsResponse {
    pub tags: Vec<QmsTag>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQmsTagRequest {
    pub tag_key: String,
    pub label: String,
    pub category: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQmsTagRequest {
    pub label: String,
    pub category: String,
}

fn bad_request(error: &'static str, message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

fn not_found(tag_key: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "qms_tag_not_found",
            message: format!("No such tag: {tag_key}"),
        }),
    )
        .into_response()
}

fn conflict(error: &'static str, message: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

/// Any authenticated caller -- catalog data, no permission gate, same
/// reasoning as `auth_roles::list_roles`. Returns every row, active or
/// not, so a future editor UI can show and reactivate a deactivated tag
/// rather than needing a second endpoint just to find it again.
pub async fn list_qms_tags(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for qms tag listing");
            return internal_error("Could not list QMS tags");
        }
    };

    let rows: Result<Vec<QmsTag>, sqlx::Error> = sqlx::query_as(
        "SELECT tag_key, label, category, is_active
           FROM client_ops.qms_tag
          ORDER BY tag_key",
    )
    .fetch_all(&mut *tx)
    .await;

    let tags = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "qms tag listing query failed");
            return internal_error("Could not list QMS tags");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit qms tag listing transaction");
        return internal_error("Could not list QMS tags");
    }

    Json(ListQmsTagsResponse { tags }).into_response()
}

pub async fn create_qms_tag(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Json(request): Json<CreateQmsTagRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "create_qms_tag", user_agent, None)
        .await
    {
        return response;
    }

    let tag_key = request.tag_key.trim().to_string();
    let label = request.label.trim().to_string();
    let category = request.category.trim().to_string();

    if tag_key.is_empty() || label.is_empty() || category.is_empty() {
        return bad_request(
            "invalid_qms_tag",
            "tag_key, label, and category are all required.".to_string(),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for qms tag creation");
            return internal_error("Could not create this tag");
        }
    };

    let insert_result = sqlx::query(
        "INSERT INTO client_ops.qms_tag (tag_key, label, category) VALUES ($1, $2, $3)",
    )
    .bind(&tag_key)
    .bind(&label)
    .bind(&category)
    .execute(&mut *tx)
    .await;

    if let Err(err) = insert_result {
        if let Err(rollback_err) = tx.rollback().await {
            tracing::error!(error = %rollback_err, "failed to roll back a failed qms tag creation");
        }

        // A duplicate tag_key is the one foreseeable conflict here — every
        // other constraint violation is a genuine internal error.
        if let sqlx::Error::Database(ref db_err) = err {
            if db_err.is_unique_violation() {
                return conflict(
                    "qms_tag_already_exists",
                    format!("A tag with key {tag_key} already exists."),
                );
            }
        }

        tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "qms tag insert failed");
        return internal_error("Could not create this tag");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "failed to commit qms tag creation");
        return internal_error("Could not create this tag");
    }

    audit_log::record(
        &state.db,
        audit_log::event::QMS_TAG_CREATED,
        user.user_id,
        ENTITY_TYPE,
        Some(&tag_key),
        Change {
            before: None,
            after: Some(serde_json::json!({ "label": label, "category": category })),
        },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, tag_key = %tag_key, "qms tag created");

    (
        StatusCode::CREATED,
        Json(QmsTag {
            tag_key,
            label,
            category,
            is_active: true,
        }),
    )
        .into_response()
}

/// Shared by `update_qms_tag` and the two activation-toggle endpoints:
/// reads the row's current state so the audit log carries a genuine
/// before/after diff rather than just the after side.
async fn fetch_current(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tag_key: &str,
) -> Result<Option<QmsTag>, sqlx::Error> {
    sqlx::query_as(
        "SELECT tag_key, label, category, is_active FROM client_ops.qms_tag WHERE tag_key = $1",
    )
    .bind(tag_key)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn update_qms_tag(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(tag_key): Path<String>,
    Json(request): Json<UpdateQmsTagRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "update_qms_tag", user_agent, None)
        .await
    {
        return response;
    }

    let label = request.label.trim().to_string();
    let category = request.category.trim().to_string();

    if label.is_empty() || category.is_empty() {
        return bad_request(
            "invalid_qms_tag",
            "label and category are both required.".to_string(),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for qms tag update");
            return internal_error("Could not update this tag");
        }
    };

    let before = match fetch_current(&mut tx, &tag_key).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing qms tag lookup");
            }
            return not_found(&tag_key);
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "qms tag lookup failed during update");
            return internal_error("Could not update this tag");
        }
    };

    if let Err(err) =
        sqlx::query("UPDATE client_ops.qms_tag SET label = $1, category = $2 WHERE tag_key = $3")
            .bind(&label)
            .bind(&category)
            .bind(&tag_key)
            .execute(&mut *tx)
            .await
    {
        tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "qms tag update failed");
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a failed qms tag update");
        }
        return internal_error("Could not update this tag");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "failed to commit qms tag update");
        return internal_error("Could not update this tag");
    }

    audit_log::record(
        &state.db,
        audit_log::event::QMS_TAG_UPDATED,
        user.user_id,
        ENTITY_TYPE,
        Some(&tag_key),
        Change {
            before: Some(serde_json::json!({ "label": before.label, "category": before.category })),
            after: Some(serde_json::json!({ "label": label, "category": category })),
        },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, tag_key = %tag_key, "qms tag updated");

    Json(QmsTag {
        tag_key,
        label,
        category,
        is_active: before.is_active,
    })
    .into_response()
}

/// Shared body for the two activation-toggle endpoints below -- never a
/// hard delete, per the design's own reasoning: a template already
/// referencing a tag must keep it resolvable (or at least visible as
/// deactivated) rather than have it vanish outright.
async fn set_active(
    state: &AppState,
    user: &AuthenticatedUser,
    user_agent: Option<&str>,
    tag_key: &str,
    active: bool,
) -> Response {
    let action = if active {
        "reactivate_qms_tag"
    } else {
        "deactivate_qms_tag"
    };

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, action, user_agent, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for qms tag activation change");
            return internal_error("Could not update this tag");
        }
    };

    let before = match fetch_current(&mut tx, tag_key).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing qms tag lookup");
            }
            return not_found(tag_key);
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "qms tag lookup failed during activation change");
            return internal_error("Could not update this tag");
        }
    };

    if before.is_active == active {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op qms tag activation change");
        }
        return conflict(
            "qms_tag_already_in_state",
            format!(
                "Tag {tag_key} is already {}.",
                if active { "active" } else { "inactive" }
            ),
        );
    }

    if let Err(err) = sqlx::query("UPDATE client_ops.qms_tag SET is_active = $1 WHERE tag_key = $2")
        .bind(active)
        .bind(tag_key)
        .execute(&mut *tx)
        .await
    {
        tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "qms tag activation update failed");
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a failed qms tag activation change");
        }
        return internal_error("Could not update this tag");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, tag_key = %tag_key, "failed to commit qms tag activation change");
        return internal_error("Could not update this tag");
    }

    let event = if active {
        audit_log::event::QMS_TAG_REACTIVATED
    } else {
        audit_log::event::QMS_TAG_DEACTIVATED
    };

    audit_log::record(
        &state.db,
        event,
        user.user_id,
        ENTITY_TYPE,
        Some(tag_key),
        Change {
            before: Some(serde_json::json!({ "is_active": before.is_active })),
            after: Some(serde_json::json!({ "is_active": active })),
        },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, tag_key = %tag_key, active, "qms tag activation changed");

    Json(QmsTag {
        tag_key: tag_key.to_string(),
        label: before.label,
        category: before.category,
        is_active: active,
    })
    .into_response()
}

pub async fn deactivate_qms_tag(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(tag_key): Path<String>,
) -> Response {
    set_active(&state, &user, request_context(&headers), &tag_key, false).await
}

pub async fn reactivate_qms_tag(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(tag_key): Path<String>,
) -> Response {
    set_active(&state, &user, request_context(&headers), &tag_key, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{
        department_manager_user, empty_state, onboarding_manager_user, test_user,
    };

    #[tokio::test]
    async fn list_requires_only_authentication_not_a_permission() {
        // test_user() carries no permissions at all -- listing must still
        // reach the database rather than 403, since reading the catalog
        // has no permission gate.
        let response = list_qms_tags(State(empty_state()), test_user()).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn create_refuses_insufficient_permission_without_touching_the_database() {
        let response = create_qms_tag(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Json(CreateQmsTagRequest {
                tag_key: "e.test".to_string(),
                label: "Test".to_string(),
                category: "Tenant".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_with_sufficient_permission_reaches_the_database() {
        // onboarding_manager holds client_ops.manage_tags per the
        // add_qms_tag_manage_permission migration -- see test_support.
        let response = create_qms_tag(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Json(CreateQmsTagRequest {
                tag_key: "e.test".to_string(),
                label: "Test".to_string(),
                category: "Tenant".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn create_rejects_blank_fields_without_touching_the_database() {
        let response = create_qms_tag(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Json(CreateQmsTagRequest {
                tag_key: "  ".to_string(),
                label: "Test".to_string(),
                category: "Tenant".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn department_manager_also_holds_manage_tags_and_reaches_the_database() {
        // All three client-ops-adjacent roles hold client_ops.manage_tags
        // per Boris's call — this confirms department_manager specifically,
        // not just onboarding_manager/admin.
        let response = create_qms_tag(
            State(empty_state()),
            department_manager_user(),
            HeaderMap::new(),
            Json(CreateQmsTagRequest {
                tag_key: "e.test".to_string(),
                label: "Test".to_string(),
                category: "Tenant".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn update_refuses_insufficient_permission_without_touching_the_database() {
        let response = update_qms_tag(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path("e.fname".to_string()),
            Json(UpdateQmsTagRequest {
                label: "First Name".to_string(),
                category: "Tenant".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn deactivate_refuses_insufficient_permission_without_touching_the_database() {
        let response = deactivate_qms_tag(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path("e.fname".to_string()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reactivate_refuses_insufficient_permission_without_touching_the_database() {
        let response = reactivate_qms_tag(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path("e.fname".to_string()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
