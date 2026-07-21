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
/// row itself; the caller is responsible for setting revoked_at there
/// too (see the logout endpoint, task 10), otherwise the token would
/// still resolve successfully if presented again by some other means.
pub fn clear_session_cookie(jar: CookieJar) -> CookieJar {
    jar.remove(Cookie::from(SESSION_COOKIE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_cookie_reads_back_the_same_token() {
        let jar = CookieJar::new();
        let jar = issue_session_cookie(
            jar,
            "raw-token-value".to_string(),
            time::Duration::hours(1),
        );

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

    #[test]
    fn cleared_cookie_no_longer_reads_back() {
        let jar = CookieJar::new();
        let jar = issue_session_cookie(
            jar,
            "raw-token-value".to_string(),
            time::Duration::hours(1),
        );
        let jar = clear_session_cookie(jar);

        assert_eq!(read_session_cookie(&jar), None);
    }
}
