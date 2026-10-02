//! Turns a ClickUp URL a person pasted ("Link manually") into the id it
//! refers to. Two shapes matter in practice:
//!
//! * `https://app.clickup.com/8413555/v/li/901418685125` -- a list URL,
//!   the trailing number is the **list id**.
//! * `https://app.clickup.com/8413555/v/l/80rbk-81594` -- what the
//!   address bar shows while you are looking at a list's *view* (the
//!   "List" tab). The trailing `80rbk-81594` is a **view id**, which has
//!   to be resolved to its parent list through the API.
//!
//! A bare list id is accepted too. Anything else (a folder, a task, a
//! doc, another site) is refused with a message that says what to paste
//! instead -- this is a human-typed field, so the error text matters.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListReference {
    ListId(String),
    ViewId(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("That does not look like a ClickUp link. Open the facility's list in ClickUp and copy the address from the browser's address bar.")]
    NotClickUp,

    #[error("That ClickUp link is not a list. Open the facility's list (not a folder, task, or doc) and copy the address from the browser's address bar.")]
    NotAList,
}

fn is_list_id(segment: &str) -> bool {
    !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit())
}

/// View ids look like `80rbk-81594` (lowercase alphanumerics, a dash,
/// digits).
fn is_view_id(segment: &str) -> bool {
    match segment.split_once('-') {
        Some((prefix, suffix)) => {
            !prefix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_alphanumeric())
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

pub fn parse_list_reference(input: &str) -> Result<ListReference, UrlError> {
    let input = input.trim();

    if is_list_id(input) {
        return Ok(ListReference::ListId(input.to_string()));
    }

    let without_scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .ok_or(UrlError::NotClickUp)?;

    let (host, rest) = without_scheme.split_once('/').ok_or(UrlError::NotClickUp)?;

    if host != "app.clickup.com" {
        return Err(UrlError::NotClickUp);
    }

    // Drop any query string / fragment ("?pr=123", "#section").
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // [team, "v", kind, ...]
    let Some(position) = segments.iter().position(|s| *s == "v") else {
        return Err(UrlError::NotAList);
    };

    match segments.get(position + 1..) {
        // .../v/li/<list id>
        Some(["li", id, ..]) if is_list_id(id) => Ok(ListReference::ListId((*id).to_string())),
        // .../v/l/li/<list id>
        Some(["l", "li", id, ..]) if is_list_id(id) => Ok(ListReference::ListId((*id).to_string())),
        // .../v/l/<view id>
        Some(["l", id, ..]) if is_view_id(id) => Ok(ListReference::ViewId((*id).to_string())),
        _ => Err(UrlError::NotAList),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_url_gives_the_list_id() {
        assert_eq!(
            parse_list_reference("https://app.clickup.com/8413555/v/li/901418685125"),
            Ok(ListReference::ListId("901418685125".to_string()))
        );
    }

    #[test]
    fn the_list_view_url_in_the_address_bar_gives_a_view_id() {
        // The exact URL Boris pasted when describing the example list.
        assert_eq!(
            parse_list_reference("https://app.clickup.com/8413555/v/l/80rbk-81594"),
            Ok(ListReference::ViewId("80rbk-81594".to_string()))
        );
    }

    #[test]
    fn the_dashboard_tab_of_a_list_is_a_view_too() {
        assert_eq!(
            parse_list_reference("https://app.clickup.com/8413555/v/l/80rbk-81614"),
            Ok(ListReference::ViewId("80rbk-81614".to_string()))
        );
    }

    #[test]
    fn a_list_url_with_the_l_li_form_gives_the_list_id() {
        assert_eq!(
            parse_list_reference("https://app.clickup.com/8413555/v/l/li/901418685125"),
            Ok(ListReference::ListId("901418685125".to_string()))
        );
    }

    #[test]
    fn query_strings_fragments_and_surrounding_whitespace_are_ignored() {
        assert_eq!(
            parse_list_reference(
                "  https://app.clickup.com/8413555/v/li/901418685125?pr=12345#top \n"
            ),
            Ok(ListReference::ListId("901418685125".to_string()))
        );
    }

    #[test]
    fn a_bare_list_id_is_accepted() {
        assert_eq!(
            parse_list_reference("901418685125"),
            Ok(ListReference::ListId("901418685125".to_string()))
        );
    }

    #[test]
    fn other_sites_are_refused() {
        for url in [
            "https://example.com/8413555/v/li/901418685125",
            "https://app.clickup.com.evil.test/8413555/v/li/1",
            "https://process.st/foo",
            "not a url",
            "",
        ] {
            assert_eq!(
                parse_list_reference(url),
                Err(UrlError::NotClickUp),
                "{url:?}"
            );
        }
    }

    #[test]
    fn folders_tasks_and_docs_are_refused_as_not_a_list() {
        for url in [
            "https://app.clickup.com/8413555/v/o/f/901410626857",
            "https://app.clickup.com/t/86bb6t0yk",
            "https://app.clickup.com/8413555/v/dc/80rbk-1049",
            "https://app.clickup.com/8413555/home",
            "https://app.clickup.com/8413555/v/li/not-a-number",
        ] {
            assert_eq!(
                parse_list_reference(url),
                Err(UrlError::NotAList),
                "{url:?}"
            );
        }
    }
}
