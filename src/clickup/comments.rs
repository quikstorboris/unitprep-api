//! Reading a task's comments: the source of ClickUp Copy's prefilled
//! comment (the source task's latest) and of its "looks already copied"
//! check on the target. Writing a comment is `ClickUpClient::add_comment`
//! in `tasks.rs`. Runs with the acting user's own token, like the rest of
//! this module.

use std::collections::HashSet;

use serde::Deserialize;

use super::client::{ClickUpClient, ClickUpError};

/// ClickUp returns at most this many comments per request.
const PAGE_SIZE: usize = 25;

/// A task with more than this many pages (100 comments) of history is
/// not something the copy flow needs to read in full; this also stops a
/// paging misunderstanding from looping.
const MAX_PAGES: usize = 4;

/// One comment, reduced to what copying needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickUpComment {
    pub id: String,
    /// The comment as plain text (formatting, mentions and attachments
    /// are flattened by ClickUp's own `comment_text`).
    pub text: String,
    pub author: String,
    /// Creation time, milliseconds since the Unix epoch.
    pub date_ms: i64,
}

#[derive(Deserialize)]
struct CommentsPage {
    #[serde(default)]
    comments: Vec<CommentBody>,
}

#[derive(Deserialize)]
struct CommentBody {
    // ClickUp sends the id and date as JSON strings or numbers depending
    // on the endpoint version; accept both.
    id: serde_json::Value,
    #[serde(default)]
    comment_text: String,
    user: Option<AuthorBody>,
    date: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AuthorBody {
    #[serde(default)]
    username: String,
}

fn scalar_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl From<CommentBody> for ClickUpComment {
    fn from(body: CommentBody) -> Self {
        Self {
            id: scalar_to_string(&body.id),
            text: body.comment_text,
            author: body.user.map(|user| user.username).unwrap_or_default(),
            date_ms: body
                .date
                .map(|date| scalar_to_string(&date))
                .and_then(|date| date.parse().ok())
                .unwrap_or(0),
        }
    }
}

/// The newest comment that has any text (an attachment-only comment has
/// none and would prefill an empty box).
pub fn latest(comments: &[ClickUpComment]) -> Option<&ClickUpComment> {
    comments
        .iter()
        .filter(|comment| !comment.text.trim().is_empty())
        .max_by_key(|comment| comment.date_ms)
}

impl ClickUpClient {
    /// `task_id`'s comments, newest first. Pages backwards from the
    /// oldest comment seen until a short page, a page with nothing new,
    /// or [`MAX_PAGES`].
    pub async fn task_comments(
        &self,
        token: &str,
        task_id: &str,
    ) -> Result<Vec<ClickUpComment>, ClickUpError> {
        let mut comments: Vec<ClickUpComment> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut path = format!("/task/{task_id}/comment");

        for _ in 0..MAX_PAGES {
            let body = self.get(token, &path).await?;
            let page: CommentsPage = serde_json::from_str(&body).map_err(ClickUpError::Parse)?;
            let received = page.comments.len();

            let fresh: Vec<ClickUpComment> = page
                .comments
                .into_iter()
                .map(ClickUpComment::from)
                .filter(|comment| seen.insert(comment.id.clone()))
                .collect();
            let added = fresh.len();
            comments.extend(fresh);

            if received < PAGE_SIZE || added == 0 {
                break;
            }
            let Some(oldest) = comments.iter().min_by_key(|comment| comment.date_ms) else {
                break;
            };
            path = format!(
                "/task/{task_id}/comment?start={}&start_id={}",
                oldest.date_ms, oldest.id
            );
        }

        comments.sort_by_key(|comment| std::cmp::Reverse(comment.date_ms));
        Ok(comments)
    }
}

#[cfg(test)]
mod tests {
    use axum::{extract::Query, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    use super::*;

    fn comment(id: &str, text: &str, date_ms: i64) -> ClickUpComment {
        ClickUpComment {
            id: id.to_string(),
            text: text.to_string(),
            author: "Ann".to_string(),
            date_ms,
        }
    }

    #[test]
    fn the_latest_comment_with_text_wins() {
        let comments = [
            comment("1", "first", 100),
            comment("3", "   ", 300),
            comment("2", "second", 200),
        ];

        assert_eq!(latest(&comments).unwrap().id, "2");
        assert!(latest(&[comment("1", "", 100)]).is_none());
    }

    #[test]
    fn ids_and_dates_are_accepted_as_strings_or_numbers() {
        let body: CommentBody = serde_json::from_value(json!({
            "id": 77, "comment_text": "hi", "user": { "username": "Ann" }, "date": "1585000000000"
        }))
        .unwrap();

        let parsed = ClickUpComment::from(body);
        assert_eq!(parsed.id, "77");
        assert_eq!(parsed.date_ms, 1_585_000_000_000);
        assert_eq!(parsed.author, "Ann");
    }

    async fn spawn(pages: fn(Option<&str>) -> Value) -> ClickUpClient {
        let app = Router::new().route(
            "/task/{id}/comment",
            get(move |Query(q): Query<HashMap<String, String>>| async move {
                Json(pages(q.get("start_id").map(String::as_str)))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        ClickUpClient::new(&format!("http://{addr}"))
    }

    #[tokio::test]
    async fn comments_come_back_newest_first() {
        let client = spawn(|_| {
            json!({ "comments": [
                { "id": "1", "comment_text": "old", "date": "100", "user": { "username": "Ann" } },
                { "id": "2", "comment_text": "new", "date": "200", "user": { "username": "Bo" } }
            ] })
        })
        .await;

        let comments = client.task_comments("pk", "t1").await.unwrap();
        assert_eq!(
            comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["2", "1"]
        );
    }

    #[tokio::test]
    async fn a_full_page_is_followed_by_the_next_older_one() {
        let client = spawn(|start_id| match start_id {
            None => json!({ "comments": (0..25).map(|i| json!({
                "id": format!("n{i}"), "comment_text": "x", "date": (1000 + i).to_string()
            })).collect::<Vec<_>>() }),
            Some(_) => json!({ "comments": [
                { "id": "old", "comment_text": "oldest", "date": "5" }
            ] }),
        })
        .await;

        let comments = client.task_comments("pk", "t1").await.unwrap();
        assert_eq!(comments.len(), 26);
        assert_eq!(comments.last().unwrap().id, "old");
    }

    #[tokio::test]
    async fn a_page_that_repeats_what_was_seen_ends_the_paging() {
        // A server that ignores `start_id` would otherwise loop to MAX_PAGES.
        let client = spawn(|_| {
            json!({ "comments": (0..25).map(|i| json!({
                "id": format!("c{i}"), "comment_text": "x", "date": (1000 + i).to_string()
            })).collect::<Vec<_>>() })
        })
        .await;

        assert_eq!(client.task_comments("pk", "t1").await.unwrap().len(), 25);
    }
}
