//! Request and response shapes for passkey registration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RegisterBeginRequest {
    /// Raw token from the invitation link. Only consulted for an
    /// unauthenticated caller: an authenticated caller's target comes from
    /// their session and any token they send is ignored outright --
    /// honouring it would let a signed-in user start a ceremony that
    /// writes a credential onto someone else's account.
    ///
    /// There is deliberately no `email` field. The bootstrap path this
    /// replaced took one, which made the endpoint answerable by anyone who
    /// could guess an address; a token is unguessable, so possession of it
    /// *is* the authorization.
    #[serde(default)]
    pub invite_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterBeginResponse {
    /// Passed straight to `navigator.credentials.create()` by the
    /// frontend. Opaque here -- produced by the backend, not shaped by
    /// this handler.
    pub challenge: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct RegisterFinishRequest {
    /// Exactly what `navigator.credentials.create()` resolved to,
    /// relayed unmodified. Verified by the backend against the stored
    /// ceremony state; never trusted here.
    pub credential: serde_json::Value,

    /// Optional human label for the new credential ("MacBook Touch ID").
    #[serde(default)]
    pub nickname: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterFinishResponse {
    pub success: bool,

    /// True when this registration also signed the caller in (invite path
    /// only -- an already-authenticated caller keeps the session they
    /// arrived with, so no new cookie is set).
    pub session_issued: bool,
}
