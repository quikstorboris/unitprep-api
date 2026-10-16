//! The Dropbox link the posted comment points at.

use crate::api::AppState;

/// Percent-encodes everything but unreserved characters (RFC 3986), for
/// a Dropbox web path.
fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The file's plain Dropbox web location -- the same `/home/<path>` form
/// the app's "Open Destination Folder" button uses, opened on the file
/// with `preview=`. Used when Dropbox will not create a share link.
fn dropbox_web_url(path: &str) -> String {
    let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
    let folder = folder
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(encode_component)
        .collect::<Vec<_>>()
        .join("/");

    format!(
        "https://www.dropbox.com/home/{folder}?preview={}",
        encode_component(file)
    )
}

/// A Dropbox link for the saved file and its kind: `shared` for a real
/// share link, `path` for the plain web path used when Dropbox will not
/// create one.
pub(super) async fn results_link(state: &AppState, path: &str) -> (String, &'static str) {
    match state.dropbox.shared_link(path).await {
        Ok(url) => (url, "shared"),
        Err(err) => {
            tracing::warn!(error = %err, path, "Dropbox share link unavailable; using the plain web path");
            (dropbox_web_url(path), "path")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_web_path_fallback_opens_the_file_in_its_folder() {
        assert_eq!(
            dropbox_web_url(
                "/QMS Onboarding/Acme & Sons/Duplicate Check/ACME_v1_pull_check_10-05-2026.xlsx"
            ),
            "https://www.dropbox.com/home/QMS%20Onboarding/Acme%20%26%20Sons/Duplicate%20Check?preview=ACME_v1_pull_check_10-05-2026.xlsx"
        );
    }

    #[test]
    fn a_path_with_no_folder_still_makes_a_link() {
        assert_eq!(
            dropbox_web_url("file.xlsx"),
            "https://www.dropbox.com/home/?preview=file.xlsx"
        );
    }
}
