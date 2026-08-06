//! Download links for `portal://file/...` markdown, rendered as a component
//! rather than a bare anchor.
//!
//! The obvious implementation — `<a href="/api/sessions/{id}/files/pull?...">` —
//! is a **top-level navigation** to a fallible API. On the happy path the
//! backend replies `Content-Disposition: attachment`, so the browser downloads
//! and stays put, and it looks correct. Every failure path instead returns an
//! `AppError` page, which the browser *renders*, throwing the user out of the
//! SPA and losing their session view.
//!
//! Those failures are not exotic. `pull_session_file` returns 503 whenever the
//! session's proxy is not connected, so **any** download link in an ended
//! session hits this, along with 404 (missing file), 504 (timeout) and 502
//! (oversized).
//!
//! So the click is intercepted and the fetch is done in-page: the bytes become
//! a Blob, an object URL, and a synthetic `<a download>` click. Nothing ever
//! navigates the top-level document, and a failure surfaces as inline text
//! instead of a raw JSON page.
//!
//! The `href` is kept on the anchor so the link still reads as a link —
//! hover/status-bar preview, "copy link address", middle-click — even though
//! the default action is prevented for ordinary clicks.

use gloo_net::http::Request;
use wasm_bindgen::JsCast;
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, MouseEvent, Url};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct PortalFileLinkProps {
    /// Resolved `/api/sessions/{id}/files/pull?path=...` URL.
    pub href: String,
    pub title: Option<String>,
    pub children: Html,
}

/// Human-readable reason for a failed pull, by status.
///
/// Mapped from `backend/src/handlers/files.rs`; the raw body is an `AppError`
/// payload that means nothing to a reader, and 503 in particular is a normal
/// consequence of the session having ended rather than anything being broken.
fn download_error_message(status: u16) -> String {
    match status {
        404 => "File not found in the session workspace.".to_string(),
        503 => "Session is not connected, so its files can't be read.".to_string(),
        504 => "Timed out reading the file from the session.".to_string(),
        502 => "The file couldn't be transferred (too large or malformed).".to_string(),
        401 | 403 => "You don't have access to this session's files.".to_string(),
        other => format!("Download failed ({other})."),
    }
}

/// Hand `bytes` to the browser as a file download without navigating.
fn trigger_blob_download(bytes: &[u8], filename: &str) -> Result<(), String> {
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array.buffer());

    let options = BlobPropertyBag::new();
    options.set_type("application/octet-stream");
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|_| "could not build the downloaded file".to_string())?;
    let object_url = Url::create_object_url_with_blob(&blob)
        .map_err(|_| "could not open the file".to_string())?;

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "no document".to_string())?;
    let anchor: HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| "could not build the download".to_string())?
        .dyn_into()
        .map_err(|_| "could not build the download".to_string())?;
    anchor.set_href(&object_url);
    anchor.set_download(filename);
    anchor.click();

    // The object URL pins the blob in memory until revoked.
    let _ = Url::revoke_object_url(&object_url);
    Ok(())
}

/// Best-effort filename from the `path` query parameter, matching what the
/// backend would have sent in `Content-Disposition` (which `fetch` can read but
/// only when the server exposes it, which it does not here).
fn filename_from_href(href: &str) -> String {
    href.split("path=")
        .nth(1)
        .and_then(|q| q.split('&').next())
        .map(|encoded| encoded.replace("%2F", "/").replace('+', " "))
        .and_then(|p| p.rsplit('/').next().map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download".to_string())
}

#[function_component(PortalFileLink)]
pub(super) fn portal_file_link(props: &PortalFileLinkProps) -> Html {
    let error = use_state(|| Option::<String>::None);
    let busy = use_state(|| false);

    let onclick = {
        let href = props.href.clone();
        let error = error.clone();
        let busy = busy.clone();
        Callback::from(move |event: MouseEvent| {
            // Leave modified clicks (open-in-new-tab etc.) to the browser.
            if event.ctrl_key() || event.meta_key() || event.shift_key() || event.alt_key() {
                return;
            }
            event.prevent_default();
            if *busy {
                return;
            }

            let href = href.clone();
            let error = error.clone();
            let busy = busy.clone();
            busy.set(true);
            error.set(None);

            wasm_bindgen_futures::spawn_local(async move {
                let outcome = match Request::get(&href).send().await {
                    Err(err) => Err(format!("Download failed: {err}")),
                    Ok(response) if !response.ok() => {
                        Err(download_error_message(response.status()))
                    }
                    Ok(response) => match response.binary().await {
                        Err(err) => Err(format!("Download failed: {err}")),
                        Ok(bytes) => trigger_blob_download(&bytes, &filename_from_href(&href))
                            .map_err(|reason| format!("Download failed: {reason}")),
                    },
                };
                if let Err(message) = outcome {
                    error.set(Some(message));
                }
                busy.set(false);
            });
        })
    };

    html! {
        <>
            <a
                href={props.href.clone()}
                title={props.title.clone()}
                class="md-link portal-file-link"
                {onclick}
            >
                { props.children.clone() }
            </a>
            if *busy {
                <span class="portal-file-status">{ "Downloading…" }</span>
            }
            if let Some(message) = (*error).as_ref() {
                <span class="portal-file-error" role="alert">{ message.clone() }</span>
            }
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_disconnected_session_to_a_non_alarming_message() {
        // 503 is the ordinary "session ended" case, not a fault worth alarming about.
        assert_eq!(
            download_error_message(503),
            "Session is not connected, so its files can't be read."
        );
    }

    #[test]
    fn unmapped_status_still_reports_the_code() {
        assert_eq!(download_error_message(418), "Download failed (418).");
    }

    #[test]
    fn filename_comes_from_the_last_path_segment() {
        assert_eq!(
            filename_from_href("/api/sessions/abc/files/pull?path=reports%2Ffinal.pdf"),
            "final.pdf"
        );
    }

    #[test]
    fn filename_falls_back_when_the_query_is_missing() {
        assert_eq!(
            filename_from_href("/api/sessions/abc/files/pull"),
            "download"
        );
    }
}
