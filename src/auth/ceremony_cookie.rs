use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

pub const CEREMONY_COOKIE_NAME: &str = "unitprep_reg_ceremony";

/// Same Secure-attribute policy as the real session cookie (see
/// session_cookie.rs) -- reads the same env var so local HTTP-only dev
/// doesn't need a second toggle for what is, from a transport-security
/// point of view, the same question.
fn cookie_is_secure() -> bool {
    std::env::var("SESSION_COOKIE_SECURE")
        .map(|value| value != "false")
        .unwrap_or(true)
}

/// Issues the short-lived cookie carrying a registration ceremony's id.
/// httpOnly for the same reason the session cookie is -- the id is
/// never something frontend JS needs to read or manage -- and given a
/// much shorter max_age than a real session, since a WebAuthn ceremony
/// is one request/response round trip through the browser's own
/// navigator.credentials.create(), not a standing login.
pub fn issue_ceremony_cookie(
    jar: CookieJar,
    ceremony_id: String,
    max_age: time::Duration,
) -> CookieJar {
    let cookie = Cookie::build((CEREMONY_COOKIE_NAME, ceremony_id))
        .http_only(true)
        .secure(cookie_is_secure())
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(max_age)
        .build();

    jar.add(cookie)
}

/// Reads the ceremony id from the request's cookies, if present.
pub fn read_ceremony_cookie(jar: &CookieJar) -> Option<String> {
    jar.get(CEREMONY_COOKIE_NAME)
        .map(|cookie| cookie.value().to_string())
}

/// Clears the ceremony cookie -- called once /auth/register/finish has
/// either consumed the ceremony successfully or determined it can't be
/// completed, so a stale ceremony id never lingers past its one
/// legitimate use.
pub fn clear_ceremony_cookie(jar: CookieJar) -> CookieJar {
    jar.remove(Cookie::from(CEREMONY_COOKIE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_cookie_reads_back_the_same_ceremony_id() {
        let jar = CookieJar::new();
        let jar =
            issue_ceremony_cookie(jar, "ceremony-123".to_string(), time::Duration::minutes(5));

        assert_eq!(read_ceremony_cookie(&jar), Some("ceremony-123".to_string()));
    }

    #[test]
    fn missing_cookie_reads_back_none() {
        let jar = CookieJar::new();
        assert_eq!(read_ceremony_cookie(&jar), None);
    }

    #[test]
    fn cleared_cookie_no_longer_reads_back() {
        let jar = CookieJar::new();
        let jar =
            issue_ceremony_cookie(jar, "ceremony-123".to_string(), time::Duration::minutes(5));
        let jar = clear_ceremony_cookie(jar);

        assert_eq!(read_ceremony_cookie(&jar), None);
    }
}
