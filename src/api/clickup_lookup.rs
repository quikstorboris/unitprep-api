//! Read-only ClickUp lookups behind the Company page's "Link ClickUp"
//! dialog: the list of every facility onboarding list (for the per-row
//! dropdown), per-facility match suggestions, and resolving a ClickUp URL
//! a person pasted ("Link manually"). Nothing here writes -- the link
//! itself is saved by `clients_clickup_links`, which re-verifies every
//! list with ClickUp rather than trusting anything the browser sends.
//!
//! All of it runs with the *calling user's* ClickUp token (see
//! `clickup_connection::load_user_token`), scoped to the one ClickUp
//! space named in `integrations.clickup_settings` (QMS Onboarding).
//! The whole space is fetched once and cached briefly per user
//! (`clickup::hierarchy`); matching happens locally.

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::clickup_connection::{clickup_client, clickup_failure_response, load_user_token};
use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::assignment::assign_unique;
use crate::clickup::hierarchy::{self, Hierarchy, HierarchyError};
use crate::clickup::matching::{Confidence, MatchIndex, Query};
use crate::clickup::url::{parse_list_reference, ListReference};
use crate::clickup::ClickUpError;

const PERMISSION: &str = "integrations.clickup";

/// How many ranked candidates are kept per facility before the
/// one-list-per-facility assignment picks among them.
const CANDIDATES_PER_FACILITY: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub struct ListOption {
    pub list_id: String,
    pub list_name: String,
    pub folder_id: String,
    pub folder_name: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct ListsResponse {
    pub lists: Vec<ListOption>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkedList {
    pub list_id: String,
    pub list_name: String,
    pub folder_name: Option<String>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct Suggestion {
    pub list: ListOption,
    pub score: f64,
    pub confidence: Confidence,
}

#[derive(Debug, Serialize)]
pub struct FacilitySuggestion {
    pub facility_id: Uuid,
    pub facility_name: String,
    /// What the facility is linked to right now, if anything.
    pub current: Option<LinkedList>,
    /// The best unclaimed match, if one clears the minimum score.
    pub suggestion: Option<Suggestion>,
}

#[derive(Debug, Serialize)]
pub struct SuggestionsResponse {
    pub facilities: Vec<FacilitySuggestion>,
}

fn space_not_found(space_name: &str) -> Response {
    not_found(
        "clickup_space_not_found",
        format!("Your ClickUp account cannot see a space named \"{space_name}\"."),
    )
}

pub(crate) async fn onboarding_space_name(
    state: &AppState,
    user: &AuthenticatedUser,
) -> Result<String, Response> {
    let fail = |err: sqlx::Error| {
        tracing::error!(error = %err, user_id = %user.user_id, "ClickUp settings read failed");
        internal_error("Could not read the ClickUp settings")
    };

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;
    let name: String = sqlx::query_scalar(
        "SELECT onboarding_space_name FROM integrations.clickup_settings WHERE id = 1",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(fail)?;
    tx.commit().await.map_err(fail)?;

    Ok(name)
}

/// The caller's ClickUp token and the onboarding hierarchy it can see,
/// or the response to return instead.
pub(crate) async fn token_and_hierarchy(
    state: &AppState,
    user: &AuthenticatedUser,
) -> Result<(String, std::sync::Arc<Hierarchy>), Response> {
    let (token, space_name) = tokio::try_join!(
        load_user_token(state, user),
        onboarding_space_name(state, user)
    )?;

    match hierarchy::cached_or_load(user.user_id, &clickup_client(state), &token, &space_name).await
    {
        Ok(hierarchy) => Ok((token, hierarchy)),
        Err(HierarchyError::SpaceNotFound(name)) => Err(space_not_found(&name)),
        Err(HierarchyError::ClickUp(err)) => Err(clickup_failure_response(state, user, &err).await),
    }
}

fn option_for(hierarchy: &Hierarchy, entry: &crate::clickup::matching::ListEntry) -> ListOption {
    ListOption {
        list_id: entry.list_id.clone(),
        list_name: entry.list_name.clone(),
        folder_id: entry.folder_id.clone(),
        folder_name: entry.folder_name.clone(),
        url: hierarchy.list_url(&entry.list_id),
    }
}

/// Every facility onboarding list in the space, folder by folder -- the
/// source for each row's "change the match" dropdown. Non-facility lists
/// (Post-Onboarding, templates, ...) are not included.
pub async fn list_clickup_lists(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "list_clickup_lists", None, None)
        .await
    {
        return response;
    }

    let (_, hierarchy) = match token_and_hierarchy(&state, &user).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let mut lists: Vec<ListOption> = hierarchy
        .facility_lists()
        .iter()
        .map(|entry| option_for(&hierarchy, entry))
        .collect();
    lists.sort_by(|a, b| {
        (a.folder_name.to_lowercase(), a.list_name.to_lowercase())
            .cmp(&(b.folder_name.to_lowercase(), b.list_name.to_lowercase()))
    });

    Json(ListsResponse { lists }).into_response()
}

#[derive(sqlx::FromRow)]
struct FacilityRow {
    id: Uuid,
    name: String,
    city: Option<String>,
    clickup_list_id: Option<String>,
    clickup_list_name: Option<String>,
    clickup_folder_name: Option<String>,
    clickup_list_url: Option<String>,
}

impl FacilityRow {
    fn current(&self) -> Option<LinkedList> {
        Some(LinkedList {
            list_id: self.clickup_list_id.clone()?,
            list_name: self.clickup_list_name.clone()?,
            folder_name: self.clickup_folder_name.clone(),
            url: self.clickup_list_url.clone()?,
        })
    }
}

/// One suggested ClickUp list per facility of `company_id`, never the
/// same list twice, each with a confidence so the dialog can flag the
/// shaky ones. Suggestions are only that: nothing is saved until the
/// person confirms.
pub async fn clickup_suggestions(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "clickup_suggestions", None, None)
        .await
    {
        return response;
    }

    let mut tx =
        try_response!(begin_for(&state, &user, "Could not load ClickUp suggestions").await);

    let company: Option<(String, Option<String>)> = match sqlx::query_as(
        "SELECT legal_name, dba_name FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "company lookup failed for ClickUp suggestions");
            return internal_error("Could not load ClickUp suggestions");
        }
    };

    let Some((legal_name, dba_name)) = company else {
        return not_found("not_found", "No such company.".to_string());
    };

    let facilities: Vec<FacilityRow> = match sqlx::query_as(
        "SELECT id, name, city, clickup_list_id, clickup_list_name, clickup_folder_name, clickup_list_url
           FROM clients.facilities WHERE company_id = $1 ORDER BY name",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup failed for ClickUp suggestions");
            return internal_error("Could not load ClickUp suggestions");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp suggestions lookup");
        return internal_error("Could not load ClickUp suggestions");
    }

    let (_, hierarchy) = match token_and_hierarchy(&state, &user).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let index = MatchIndex::new(hierarchy.facility_lists());

    let mut company_names: Vec<&str> = vec![legal_name.as_str()];
    if let Some(dba) = dba_name.as_deref() {
        company_names.push(dba);
    }

    let ranked = facilities
        .iter()
        .map(|facility| {
            let candidates = index.rank(
                &Query {
                    facility_name: &facility.name,
                    city: facility.city.as_deref(),
                    company_names: &company_names,
                },
                CANDIDATES_PER_FACILITY,
            );
            (facility.id, candidates)
        })
        .collect();

    let mut assigned = assign_unique(ranked);

    let response = SuggestionsResponse {
        facilities: facilities
            .iter()
            .map(|facility| FacilitySuggestion {
                facility_id: facility.id,
                facility_name: facility.name.clone(),
                current: facility.current(),
                suggestion: assigned.remove(&facility.id).flatten().map(|m| Suggestion {
                    list: option_for(&hierarchy, &m.entry),
                    score: m.score,
                    confidence: m.confidence,
                }),
            })
            .collect(),
    };

    Json(response).into_response()
}

/// Looks `list_id` up in ClickUp and confirms it is in the onboarding
/// space. Shared by "Link manually" (to show the list's real name before
/// it counts) and by saving a link (so the server never trusts a name or
/// URL sent by the browser).
pub(crate) async fn verify_list(
    state: &AppState,
    user: &AuthenticatedUser,
    token: &str,
    hierarchy: &Hierarchy,
    list_id: &str,
) -> Result<ListOption, Response> {
    // A list the (just fetched) onboarding hierarchy already holds is
    // confirmed from it: same ClickUp data, no extra call per list. Only
    // lists it does not hold (a pasted URL for a folderless list, or one
    // in another space) are asked of ClickUp.
    if let Some((folder, list)) = hierarchy.find_list(list_id) {
        return Ok(ListOption {
            list_id: list.id.clone(),
            list_name: list.name.trim().to_string(),
            folder_id: folder.id.clone(),
            folder_name: folder.name.trim().to_string(),
            url: hierarchy.list_url(&list.id),
        });
    }

    let detail = match clickup_client(state).list(token, list_id).await {
        Ok(detail) => detail,
        Err(ClickUpError::NotFound) => {
            return Err(not_found(
                "clickup_list_not_found",
                "ClickUp could not find that list, or your account cannot see it.".to_string(),
            ));
        }
        Err(err) => return Err(clickup_failure_response(state, user, &err).await),
    };

    if detail.space_id.as_deref() != Some(hierarchy.space_id.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "clickup_list_outside_onboarding_space",
                message: "That list is not in the QMS Onboarding space. Only onboarding lists can be linked.".to_string(),
            }),
        )
            .into_response());
    }

    Ok(ListOption {
        list_id: detail.id.clone(),
        list_name: detail.name.trim().to_string(),
        folder_id: detail.folder_id.unwrap_or_default(),
        folder_name: detail.folder_name.unwrap_or_default().trim().to_string(),
        url: hierarchy.list_url(&detail.id),
    })
}

#[derive(Debug, Deserialize)]
pub struct ResolveUrlRequest {
    pub url: String,
}

/// "Link manually": turns a pasted ClickUp URL into the list it points
/// at, so the dialog can show the list's real name for the person to
/// confirm. Accepts a list URL or the list *view* URL shown in the
/// address bar (see `clickup::url`).
pub async fn resolve_clickup_url(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ResolveUrlRequest>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "resolve_clickup_url", None, None)
        .await
    {
        return response;
    }

    let reference = match parse_list_reference(&request.url) {
        Ok(reference) => reference,
        Err(err) => return bad_request("invalid_clickup_url", err.to_string()),
    };

    let (token, hierarchy) = match token_and_hierarchy(&state, &user).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let list_id = match reference {
        ListReference::ListId(id) => id,
        ListReference::ViewId(view_id) => {
            match clickup_client(&state)
                .list_id_for_view(&token, &view_id)
                .await
            {
                Ok(Some(id)) => id,
                Ok(None) => {
                    return bad_request(
                        "invalid_clickup_url",
                        "That ClickUp link is not a list. Open the facility's list and copy the address from the browser's address bar.".to_string(),
                    );
                }
                Err(ClickUpError::NotFound) => {
                    return not_found(
                        "clickup_list_not_found",
                        "ClickUp could not find that list, or your account cannot see it."
                            .to_string(),
                    );
                }
                Err(err) => return clickup_failure_response(&state, &user, &err).await,
            }
        }
    };

    match verify_list(&state, &user, &token, &hierarchy, &list_id).await {
        Ok(option) => Json(option).into_response(),
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {

    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{clickup_user, empty_state, test_user};

    #[tokio::test]
    async fn listing_refuses_a_caller_without_the_permission() {
        let response = list_clickup_lists(State(empty_state()), test_user()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn suggestions_refuse_a_caller_without_the_permission() {
        let response =
            clickup_suggestions(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn resolving_refuses_a_caller_without_the_permission() {
        let response = resolve_clickup_url(
            State(empty_state()),
            test_user(),
            Json(ResolveUrlRequest {
                url: "https://app.clickup.com/1/v/li/2".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn resolving_rejects_a_non_clickup_link_before_calling_clickup() {
        let response = resolve_clickup_url(
            State(empty_state()),
            clickup_user(),
            Json(ResolveUrlRequest {
                url: "https://example.com/not-clickup".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolving_a_folder_link_is_rejected_as_not_a_list() {
        let response = resolve_clickup_url(
            State(empty_state()),
            clickup_user(),
            Json(ResolveUrlRequest {
                url: "https://app.clickup.com/8413555/v/o/f/901410626857".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
