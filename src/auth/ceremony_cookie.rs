use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

/// Carries an in-progress *registration* ceremony's id.
pub const REGISTRATION_CEREMONY_COOKIE: &str = "unitprep_reg_ceremony";

/// Carries an in-progress *login* ceremony's id.
///
/// Deliberately a different name from the registration cookie rather than
/// one shared "ceremony" cookie: the two ceremonies have separate
/// server-side stores and can legitimately be in flight at the same time
/// (a signed-in user adding a passkey in one tab while a login runs in
/// another). One shared name would let the second `begin` silently
/// overwrite the first ceremony's id, stranding it until it expired.
pub const LOGIN_CEREMONY_COOKIE: &str = "unitprep_login_ceremony";

/// Same Secure-attribute policy as the real session cookie (see
/// session_cookie.rs) -- reads the same env var so local HTTP-only dev
/// doesn't need a second toggle for what is, from a transport-security
/// point of view, the same question.
fn cookie_is_secure() -> bool {
    std::env::var("SESSION_COOKIE_SECURE")
        .map(|value| value != "false")
        .unwrap_or(true)
}

/// Issues the short-lived cookie carrying a ceremony's id, under whichever
/// of the two names the caller names explicitly.
///
/// httpOnly for the same reason the session cookie is -- the id is never
/// something frontend JS needs to read or manage -- and given a much
/// shorter max_age than a real session, since a WebAuthn ceremony is one
/// request/response round trip through the browser's own
/// `navigator.credentials` call, not a standing login.
pub fn issue_ceremony_cookie(
    jar: CookieJar,
    name: &'static str,
    ceremony_id: String,
    max_age: time::Duration,
) -> CookieJar {
    let cookie = Cookie::build((name, ceremony_id))
        .http_only(true)
        .secure(cookie_is_secure())
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(max_age)
        .build();

    jar.add(cookie)
}

/// Reads a ceremony id from the request's cookies, if present.
pub fn read_ceremony_cookie(jar: &CookieJar, name: &'static str) -> Option<String> {
    jar.get(name).map(|cookie| cookie.value().to_string())
}

/// Clears a ceremony cookie -- called once the matching `finish` endpoint
/// has either consumed the ceremony successfully or determined it cannot
/// be completed, so a stale ceremony id never lingers past its one
/// legitimate use.
pub fn clear_ceremony_cookie(jar: CookieJar, name: &'static str) -> CookieJar {
    jar.remove(Cookie::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_cookie_reads_back_the_same_ceremony_id() {
        let jar = CookieJar::new();
        let jar = issue_ceremony_cookie(
            jar,
            REGISTRATION_CEREMONY_COOKIE,
            "ceremony-123".to_string(),
            time::Duration::minutes(5),
        );

        assert_eq!(
            read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE),
            Some("ceremony-123".to_string())
        );
    }

    #[test]
    fn missing_cookie_reads_back_none() {
        let jar = CookieJar::new();
        assert_eq!(
            read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE),
            None
        );
    }

    #[test]
    fn cleared_cookie_no_longer_reads_back() {
        let jar = CookieJar::new();
        let jar = issue_ceremony_cookie(
            jar,
            REGISTRATION_CEREMONY_COOKIE,
            "ceremony-123".to_string(),
            time::Duration::minutes(5),
        );
        let jar = clear_ceremony_cookie(jar, REGISTRATION_CEREMONY_COOKIE);

        assert_eq!(
            read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE),
            None
        );
    }

    /// The whole reason the two names exist: a login ceremony must not be
    /// readable as, or clobbered by, a registration ceremony. A single
    /// shared cookie name would make these two assertions fail.
    #[test]
    fn the_two_ceremony_cookies_are_independent() {
        let jar = CookieJar::new();
        let jar = issue_ceremony_cookie(
            jar,
            REGISTRATION_CEREMONY_COOKIE,
            "reg-1".to_string(),
            time::Duration::minutes(5),
        );
        let jar = issue_ceremony_cookie(
            jar,
            LOGIN_CEREMONY_COOKIE,
            "login-1".to_string(),
            time::Duration::minutes(5),
        );

        assert_eq!(
            read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE),
            Some("reg-1".to_string()),
            "issuing a login ceremony must not disturb a registration one"
        );
        assert_eq!(
            read_ceremony_cookie(&jar, LOGIN_CEREMONY_COOKIE),
            Some("login-1".to_string())
        );

        // Clearing one must leave the other intact.
        let jar = clear_ceremony_cookie(jar, LOGIN_CEREMONY_COOKIE);
        assert_eq!(read_ceremony_cookie(&jar, LOGIN_CEREMONY_COOKIE), None);
        assert_eq!(
            read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE),
            Some("reg-1".to_string()),
            "clearing the login ceremony must not clear the registration one"
        );
    }
}
