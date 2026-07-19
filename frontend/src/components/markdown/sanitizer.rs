use uuid::Uuid;

use super::links::is_valid_url;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LinkDestination {
    PortalDownload(String),
    LiteralAngleText,
    ExternalOrRelative(String),
}

pub(super) fn classify_link_destination(href: &str, session_id: Option<Uuid>) -> LinkDestination {
    if href.starts_with("portal://file/") {
        let Some(session_id) = session_id else {
            return LinkDestination::LiteralAngleText;
        };
        if let Some(download_href) = portal_file_download_href(href, session_id) {
            LinkDestination::PortalDownload(download_href)
        } else {
            LinkDestination::LiteralAngleText
        }
    } else if !is_valid_url(href) && !href.starts_with('#') && !href.starts_with('/') {
        LinkDestination::LiteralAngleText
    } else {
        LinkDestination::ExternalOrRelative(href.to_string())
    }
}

/// Keep raw markdown HTML visible without inserting it as trusted DOM.
///
/// The renderer emits this return value through Yew text nodes, so tags such as
/// `<script>` render literally instead of executing. Keeping this as a
/// sanitizer boundary makes that contract explicit for future richer handling.
pub(super) fn sanitize_raw_html(html_text: &str) -> String {
    html_text.to_string()
}

fn portal_file_download_href(href: &str, session_id: Uuid) -> Option<String> {
    let path = href.strip_prefix("portal://file/")?;
    if path.is_empty() {
        return None;
    }
    let encoded = encode_uri_component(path);
    Some(format!(
        "/api/sessions/{}/files/pull?path={}",
        session_id, encoded
    ))
}

fn encode_uri_component(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => encoded.push(byte as char),
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{:02X}", byte));
            }
        }
    }
    encoded
}
