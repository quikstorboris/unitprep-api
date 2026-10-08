//! Opening the caller's row-level-security transaction from a handler.
//!
//! Almost every handler begins with the same lines: open a transaction with
//! the caller's identity and roles set (`auth::begin_rls_transaction`), and
//! if that fails, log it and answer 500. [`begin_for`] is that sequence, and
//! [`try_response!`] is the `match { Ok(v) => v, Err(response) => return
//! response }` that turns its error into the handler's early return.

use axum::response::Response;
use sqlx::{Postgres, Transaction};

use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

/// Opens `user`'s RLS transaction. On failure logs the cause and returns the
/// 500 the handler should answer with; `context` is the caller-facing
/// message ("Could not load clients") and is also logged, so the log line
/// says which endpoint it came from.
pub(crate) async fn begin_for<'a>(
    state: &'a AppState,
    user: &AuthenticatedUser,
    context: &str,
) -> Result<Transaction<'a, Postgres>, Response> {
    match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => Ok(tx),
        Err(err) => {
            tracing::error!(
                error = %err,
                user_id = %user.user_id,
                context,
                "failed to open the request's RLS transaction"
            );
            Err(internal_error(context))
        }
    }
}

/// Unwraps a `Result<T, Response>`, returning the `Response` from the
/// enclosing handler on `Err`. For handlers whose return type is `Response`.
macro_rules! try_response {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(response) => return response,
        }
    };
}

pub(crate) use try_response;
