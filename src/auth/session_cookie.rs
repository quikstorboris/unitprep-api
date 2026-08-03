use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

pub const SESSION_COOKIE_NAME: &str = "unitprep_session";

/// Whether the session cookie gets the Secure attribute (HTTPS-only
/// transport). Defaults to true -- disable only for local HTTP-only
/// dev if that turns out to be necessary once the full login flow is
/// actually exercised end-to-end; not verified either way yet.
fn cookie_is_secure() -> bool {
    std::env::var("SESSION_COOKIE_SECURE")
        .map(|value| value != "false")
        .unwrap_or(true)
}

/// True when `origin` names localhost by IP or hostname, over any scheme
/// or port -- the one case where an insecure session cookie is expected
/// and harmless (WebAuthn itself already treats localhost as a secure
/// context, which is why local dev works without HTTPS at all).
fn is_localhost_origin(origin: &str) -> bool {
    origin
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', ':']).next())
        .is_some_and(|host| host == "localhost" || host == "127.0.0.1")
}

/// Refuses to start with a real, non-localhost origin serving an
/// insecure session cookie -- that combination means every session token
/// travels in plaintext over the network, not just on a developer's own
/// machine. `SESSION_COOKIE_SECURE=false` exists as a local-HTTP-dev
/// escape hatch (see `cookie_is_secure` above); this is what stops that
/// escape hatch from surviving unnoticed into a real deployment. Called
/// once from `main.rs` at startup, alongside the other fatal
/// misconfiguration checks (database pool, WebAuthn backend).
pub fn validate_cookie_security(rp_origin: &str) -> Result<(), String> {
    if !cookie_is_secure() && !is_localhost_origin(rp_origin) {
        return Err(format!(
            "SESSION_COOKIE_SECURE=false with a non-localhost WEBAUTHN_RP_ORIGIN \
             ({rp_origin}) -- session cookies would travel over plain HTTP outside \
             local dev. Set SESSION_COOKIE_SECURE=true (the default -- just unset \
             it) or point WEBAUTHN_RP_ORIGIN at localhost."
        ));
    }

    Ok(())
}

/// Builds the Set-Cookie response for a freshly issued session --
/// httpOnly (unreadable to page JS, so an XSS bug cannot exfiltrate
/// it), SameSite=Lax (sent on top-level navigation, not on cross-site
/// subrequests), and Secure per cookie_is_secure above. Deliberately
/// not signed or encrypted -- the token itself is opaque random data,
/// not a claim we would trust without a database round-trip through
/// resolve_session, so there is nothing here worth protecting beyond
/// transport and JS-readability.
pub fn issue_session_cookie(
    jar: CookieJar,
    raw_token: String,
    max_age: time::Duration,
) -> CookieJar {
    let cookie = Cookie::build((SESSION_COOKIE_NAME, raw_token))
        .http_only(true)
        .secure(cookie_is_secure())
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(max_age)
        .build();

    jar.add(cookie)
}

/// Reads the raw session token from the request's cookies, if present.
pub fn read_session_cookie(jar: &CookieJar) -> Option<String> {
    jar.get(SESSION_COOKIE_NAME)
        .map(|cookie| cookie.value().to_string())
}

/// Clears the session cookie -- logout. Does not touch the sessions
/// row itself; the caller is responsible for calling `auth.revoke_session`
/// too (see `api::auth_logout`), otherwise the token would still resolve
/// successfully if presented again by some other means.
///
/// ## The Path is load-bearing, and its absence was a real bug
///
/// A browser matches a deletion against a stored cookie by **name, domain
/// and path**. Per RFC 6265 a `Set-Cookie` with no `Path` attribute does not
/// mean "all paths" -- it defaults to the *directory of the requesting URI*.
/// So clearing from `POST /auth/logout` without an explicit path emitted a
/// deletion scoped to `/auth`, which does not match the real cookie's
/// `Path=/`, and the browser kept it.
///
/// The failure mode is quiet rather than dangerous: the session is genuinely
/// revoked server-side, so nothing is exposed. But the browser goes on
/// presenting a dead token, every subsequent request 401s with a cookie
/// attached, and it reads like a broken session rather than a completed
/// logout.
///
/// It survived because `CookieJar::remove` on an in-memory jar simply drops
/// the entry -- it models no path semantics at all -- so a unit test
/// asserting "the cookie no longer reads back" passes either way. The tests
/// below therefore assert the emitted **attributes**, which is the only part
/// a browser actually consults.
///
/// ## Why this adds an expired cookie instead of calling `remove`
///
/// `cookie::CookieJar::remove` produces a removal in the delta **only if a
/// cookie of that name was an *original*** in the jar -- i.e. one parsed
/// from the request. Remove something that was merely added, or was never
/// there, and nothing is emitted at all.
///
/// For a logout endpoint that is the wrong default twice over. A browser can
/// be holding a cookie the server did not receive -- scoped to a path that
/// did not match this request, for instance, which is exactly the bug fixed
/// above -- and that is precisely the case where telling it to drop the
/// cookie matters most. Adding an already-expired cookie emits the
/// `Set-Cookie` unconditionally, so "logout always clears" is true rather
/// than true-when-the-jar-happened-to-parse-it.
///
/// The consequence for callers: after this, `read_session_cookie` sees an
/// empty value rather than nothing. **Read the token before clearing** --
/// see `api::auth_logout`, which does.
pub fn clear_session_cookie(jar: CookieJar) -> CookieJar {
    // Attributes mirror `issue_session_cookie` deliberately. Only name,
    // domain and path participate in matching, but keeping the pair
    // identical means a future change to how the cookie is issued is
    // visibly a change to how it is cleared.
    let expired = Cookie::build((SESSION_COOKIE_NAME, ""))
        .http_only(true)
        .secure(cookie_is_secure())
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::ZERO)
        .build();

    jar.add(expired)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-var tests here follow the same set-then-remove pattern as
    /// `auth_register.rs`'s bootstrap-env tests -- `cookie_is_secure`
    /// reads `SESSION_COOKIE_SECURE` directly, so exercising
    /// `validate_cookie_security`'s branches means setting it for the
    /// duration of one assertion and removing it immediately after.
    #[test]
    fn localhost_origin_is_allowed_even_with_an_insecure_cookie() {
        std::env::set_var("SESSION_COOKIE_SECURE", "false");
        let result = validate_cookie_security("http://localhost:3000");
        std::env::remove_var("SESSION_COOKIE_SECURE");

        assert!(
            result.is_ok(),
            "localhost must remain usable for local HTTP dev"
        );
    }

    #[test]
    fn a_real_origin_with_an_insecure_cookie_is_refused() {
        std::env::set_var("SESSION_COOKIE_SECURE", "false");
        let result = validate_cookie_security("https://app.example.com");
        std::env::remove_var("SESSION_COOKIE_SECURE");

        assert!(
            result.is_err(),
            "a non-localhost origin must never pair with an insecure session cookie"
        );
    }

    #[test]
    fn a_real_origin_with_the_default_secure_cookie_is_allowed() {
        std::env::remove_var("SESSION_COOKIE_SECURE");
        assert!(validate_cookie_security("https://app.example.com").is_ok());
    }

    #[test]
    fn loopback_ip_origin_counts_as_localhost() {
        std::env::set_var("SESSION_COOKIE_SECURE", "false");
        let result = validate_cookie_security("http://127.0.0.1:3000");
        std::env::remove_var("SESSION_COOKIE_SECURE");

        assert!(result.is_ok());
    }

    #[test]
    fn issued_cookie_reads_back_the_same_token() {
        let jar = CookieJar::new();
        let jar =
            issue_session_cookie(jar, "raw-token-value".to_string(), time::Duration::hours(1));

        assert_eq!(
            read_session_cookie(&jar),
            Some("raw-token-value".to_string())
        );
    }

    #[test]
    fn missing_cookie_reads_back_none() {
        let jar = CookieJar::new();
        assert_eq!(read_session_cookie(&jar), None);
    }

    /// After clearing, no usable token remains readable.
    ///
    /// Asserts an *empty* value rather than `None`, which is the honest
    /// post-condition now that clearing adds an expired cookie rather than
    /// removing an entry. The previous version of this test asserted `None`
    /// and would have hidden the change -- and the reason it matters is that
    /// a caller which clears before reading gets an empty string, not
    /// nothing, so it must read first. See `api::auth_logout`.
    #[test]
    fn cleared_cookie_leaves_no_usable_token() {
        let jar = CookieJar::new();
        let jar =
            issue_session_cookie(jar, "raw-token-value".to_string(), time::Duration::hours(1));
        let jar = clear_session_cookie(jar);

        assert_eq!(read_session_cookie(&jar).as_deref(), Some(""));
    }

    /// The deletion header must be emitted even when the request carried no
    /// session cookie at all. This is the case `remove()` silently skipped:
    /// a browser holding a cookie the server did not parse is exactly who
    /// needs telling to drop it.
    #[test]
    fn clearing_emits_a_header_even_with_nothing_to_clear() {
        let line = set_cookie_line(clear_session_cookie(CookieJar::new()));

        assert_eq!(path_attribute(&line), Some("/"), "emitted: {line}");
        assert!(
            line.contains("Max-Age=0"),
            "the cleared cookie must expire immediately -- emitted: {line}"
        );
    }

    /// Renders a jar the way axum will, and returns the `Set-Cookie` line for
    /// the session cookie.
    ///
    /// Inspecting the emitted header rather than the jar's contents is the
    /// whole point. `cleared_cookie_no_longer_reads_back` above passed
    /// against an implementation that emitted **no Path at all**, because an
    /// in-memory jar models no path semantics -- it simply drops the entry.
    /// Only what goes on the wire tells you what a browser will do.
    fn set_cookie_line(jar: CookieJar) -> String {
        use axum::response::IntoResponse;

        let response = (jar, axum::http::StatusCode::OK).into_response();

        response
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(SESSION_COOKIE_NAME))
            .map(str::to_string)
            .expect("a Set-Cookie for the session cookie must be emitted")
    }

    fn path_attribute(set_cookie: &str) -> Option<&str> {
        set_cookie
            .split(';')
            .map(str::trim)
            .find_map(|part| part.strip_prefix("Path="))
    }

    /// The removal must carry `Path=/`, matching what `issue_session_cookie`
    /// set, or a browser will not match it and keeps the cookie.
    #[test]
    fn the_removal_cookie_carries_a_root_path() {
        let line = set_cookie_line(clear_session_cookie(CookieJar::new()));

        assert_eq!(
            path_attribute(&line),
            Some("/"),
            "a removal without Path=/ defaults to the request's directory and will not \
             match a cookie set at the root -- emitted: {line}"
        );
    }

    /// Whatever `issue_session_cookie` uses as the path, clearing must use
    /// the same one. A comparison rather than two hardcoded assertions, so
    /// changing the issued path without changing the cleared path fails here
    /// rather than shipping a logout that silently stops working.
    #[test]
    fn issued_and_cleared_paths_agree() {
        let issued = set_cookie_line(issue_session_cookie(
            CookieJar::new(),
            "raw-token-value".to_string(),
            time::Duration::hours(1),
        ));
        let cleared = set_cookie_line(clear_session_cookie(CookieJar::new()));

        assert_eq!(
            path_attribute(&issued),
            path_attribute(&cleared),
            "issued: {issued}\ncleared: {cleared}"
        );
    }

    /// A removal must not carry a usable value. Belt-and-braces against an
    /// implementation that "clears" by setting a new token.
    #[test]
    fn the_removal_cookie_has_no_value() {
        let line = set_cookie_line(clear_session_cookie(CookieJar::new()));
        let value = line
            .split(';')
            .next()
            .and_then(|first| first.split_once('='))
            .map(|(_, value)| value)
            .expect("the Set-Cookie line must have a name=value pair");

        assert_eq!(value, "", "emitted: {line}");
    }
}
