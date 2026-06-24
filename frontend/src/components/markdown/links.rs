use uuid::Uuid;
use yew::prelude::*;

/// Convert raw URLs in text to clickable links.
/// Handles http:// and https:// URLs that aren't already in markdown link syntax.
pub fn linkify_urls(text: &str) -> Html {
    let mut parts: Vec<Html> = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next URL
        if let Some((before, url, after)) = find_next_url(remaining) {
            // Add text before the URL
            if !before.is_empty() {
                parts.push(html! { <>{ before.to_string() }</> });
            }
            // Add the URL as a link
            parts.push(html! {
                <a href={url.to_string()} target="_blank" rel="noopener noreferrer" class="md-link">
                    { url }
                </a>
            });
            remaining = after;
        } else {
            // No more URLs, add remaining text
            parts.push(html! { <>{ remaining.to_string() }</> });
            break;
        }
    }

    html! { <>{ for parts }</> }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LinkDestination {
    PortalDownload(String),
    LiteralAngleText,
    ExternalOrRelative(String),
}

pub(super) fn classify_link_destination(href: &str, session_id: Option<Uuid>) -> LinkDestination {
    if href.starts_with("portal://file/") {
        let session_id =
            session_id.expect("portal://file markdown links require a session_id to render");
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

/// Find the next URL in text, returning (text_before, url, text_after).
pub(super) fn find_next_url(text: &str) -> Option<(&str, &str, &str)> {
    // Find http:// or https://
    let https_pos = text.find("https://");
    let http_pos = text.find("http://");

    let start = match (https_pos, http_pos) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }?;

    let before = &text[..start];
    let url_start = &text[start..];

    // Find where the URL ends
    let url_end = find_url_end(url_start);
    let url = trim_url_punctuation(&url_start[..url_end]);

    // Validate it looks like a real URL
    if !is_valid_url(url) {
        // Not a valid URL, skip this match and try to find the next one
        let skip = start + 1;
        if skip < text.len() {
            return find_next_url(&text[skip..]).map(|(b, u, a)| {
                // Adjust the "before" to include the skipped text
                let new_before_end = start + 1 + b.len();
                (&text[..new_before_end], u, a)
            });
        }
        return None;
    }

    let after = &text[start + url.len()..];
    Some((before, url, after))
}

/// Find where a URL ends (whitespace or certain punctuation).
fn find_url_end(text: &str) -> usize {
    let mut end = 0;
    let mut paren_depth = 0;
    let mut bracket_depth = 0;

    for c in text.chars() {
        match c {
            // Whitespace ends URL
            ' ' | '\t' | '\n' | '\r' => break,
            // Track parentheses for Wikipedia-style URLs
            '(' => {
                paren_depth += 1;
                end += c.len_utf8();
            }
            ')' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            // Track brackets
            '[' => {
                bracket_depth += 1;
                end += c.len_utf8();
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            // Common URL-safe characters
            'a'..='z'
            | 'A'..='Z'
            | '0'..='9'
            | '-'
            | '_'
            | '.'
            | '~'
            | '/'
            | '?'
            | '#'
            | '&'
            | '='
            | '+'
            | '%'
            | '@'
            | ':'
            | '!'
            | '$'
            | '\''
            | '*'
            | ',' => {
                end += c.len_utf8();
            }
            // Stop on other characters (like < > " etc)
            _ => break,
        }
    }

    end
}

/// Trim trailing punctuation that's commonly not part of URLs.
fn trim_url_punctuation(url: &str) -> &str {
    let mut url = url;
    let trim_chars = ['.', ',', '!', '?', ';', ':', '"', '\''];

    while let Some(c) = url.chars().last() {
        // Handle unbalanced closing parens/brackets
        if c == ')' {
            let open = url.chars().filter(|&ch| ch == '(').count();
            let close = url.chars().filter(|&ch| ch == ')').count();
            if close > open {
                url = &url[..url.len() - 1];
                continue;
            }
            break;
        }
        if c == ']' {
            let open = url.chars().filter(|&ch| ch == '[').count();
            let close = url.chars().filter(|&ch| ch == ']').count();
            if close > open {
                url = &url[..url.len() - 1];
                continue;
            }
            break;
        }
        // Trim common trailing punctuation
        if trim_chars.contains(&c) {
            url = &url[..url.len() - c.len_utf8()];
        } else {
            break;
        }
    }
    url
}

/// Check if a URL looks valid (has domain with dot or is localhost).
pub(super) fn is_valid_url(url: &str) -> bool {
    let after_protocol = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");

    if after_protocol.is_empty() {
        return false;
    }

    // Extract domain (before first /)
    let domain_end = after_protocol.find('/').unwrap_or(after_protocol.len());
    let domain = &after_protocol[..domain_end];

    // Must have a dot or be localhost
    domain.contains('.') || domain.starts_with("localhost")
}
