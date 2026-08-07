//! Media renderers shared by the assistant and portal families: the image
//! lightbox, the `agent-portal show` video player, and the expired-blob
//! placeholder both degrade to.

use crate::hooks::use_escape_capture;
use yew::prelude::*;

const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

pub(super) fn render_image_source(source: &shared::ImageSource, filename: Option<String>) -> Html {
    if !ALLOWED_IMAGE_MEDIA_TYPES.contains(&source.media_type.as_str()) {
        return html! {
            <pre class="tool-result-content">
                { format!("[unsupported image type: {}]", source.media_type) }
            </pre>
        };
    }
    // Support both URL sources (from backend image store) and base64 data URIs
    let src = if source.source_type.as_str() == "url" {
        source.data.clone()
    } else {
        format!("data:{};base64,{}", source.media_type, source.data)
    };
    html! {
        <ImageViewer src={src} media_type={source.media_type.as_str().to_string()} {filename} />
    }
}

#[derive(Properties, PartialEq)]
struct ImageViewerProps {
    pub src: String,
    pub media_type: String,
    #[prop_or_default]
    pub filename: Option<String>,
}

/// Does this media type need a CSS width fallback to be visible?
///
/// Raster formats always carry intrinsic pixel dimensions, but an SVG may
/// declare none — a bare `viewBox`, or percentage `width`/`height`, is common
/// in hand-authored diagrams (matplotlib is fine; it emits `width`/`height` in
/// points). Such an image has *only* an aspect ratio, so inside the
/// shrink-to-fit `.tool-result-image` frame the frame's width depends on the
/// image and the image's `max-width: 100%` depends on the frame. Nothing can
/// resolve that cycle and browsers collapse it to **0×0** — the diagram
/// silently vanishes, leaving just the frame's 1px border.
///
/// Tagging those elements with `svg` lets CSS supply a definite width basis.
/// Don't drop this without re-testing a `viewBox`-only SVG: it fails silently
/// (no `onerror`, no console warning), so it's easy to regress unnoticed.
/// See `.tool-result-image.svg` / `.image-lightbox-content img.svg` in
/// `frontend/styles/markdown.css`.
fn needs_size_fallback(media_type: &str) -> bool {
    media_type == "image/svg+xml"
}

#[function_component(ImageViewer)]
fn image_viewer(props: &ImageViewerProps) -> Html {
    let expanded = use_state(|| false);
    // The bytes behind a served-image URL are TTL/LRU-bounded, so a persisted
    // transcript row can outlive them. When the <img> fails to load, degrade to
    // a "media expired" placeholder rather than a broken image icon.
    let failed = use_state(|| false);

    // Close lightbox on Escape key (capture phase so it doesn't trigger nav mode)
    {
        let expanded = expanded.clone();
        use_escape_capture(*expanded, Callback::from(move |()| expanded.set(false)));
    }

    if *failed {
        return render_media_expired(props.filename.as_deref(), "image");
    }

    let on_error = {
        let failed = failed.clone();
        Callback::from(move |_: Event| failed.set(true))
    };

    let on_thumb_click = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(true))
    };

    let on_close = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(false))
    };

    let ext = match props.media_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "bin",
    };

    let download_name = props
        .filename
        .clone()
        .unwrap_or_else(|| format!("image.{ext}"));

    let size_fallback = needs_size_fallback(&props.media_type).then_some("svg");

    html! {
        <>
            <div class={classes!("tool-result-image", size_fallback)} onclick={on_thumb_click}>
                <img src={props.src.clone()} alt="Tool result image" onerror={on_error} />
            </div>
            if *expanded {
                <div class="image-lightbox" onclick={on_close.clone()}>
                    <div class="image-lightbox-content" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <img class={classes!(size_fallback)} src={props.src.clone()} alt="Full size image" />
                        <div class="image-lightbox-controls">
                            <a
                                class="image-lightbox-download"
                                href={props.src.clone()}
                                download={download_name}
                            >
                                { "Download" }
                            </a>
                            <button class="image-lightbox-close" onclick={on_close}>
                                { "\u{00d7}" }
                            </button>
                        </div>
                    </div>
                </div>
            }
        </>
    }
}

const ALLOWED_VIDEO_MEDIA_TYPES: &[&str] = &["video/mp4", "video/webm"];

/// Render a video shown via `agent-portal show`. `url` is always a served-media
/// URL (`/api/media/{id}`); the bytes are TTL/size-bounded, so `VideoViewer`
/// degrades to a placeholder when the URL 404s.
pub(super) fn render_video_source(media_type: &str, url: &str, filename: Option<String>) -> Html {
    if !ALLOWED_VIDEO_MEDIA_TYPES.contains(&media_type) {
        return html! {
            <pre class="tool-result-content">
                { format!("[unsupported video type: {media_type}]") }
            </pre>
        };
    }
    html! {
        <VideoViewer src={url.to_string()} {filename} />
    }
}

#[derive(Properties, PartialEq)]
struct VideoViewerProps {
    pub src: String,
    #[prop_or_default]
    pub filename: Option<String>,
}

#[function_component(VideoViewer)]
fn video_viewer(props: &VideoViewerProps) -> Html {
    let failed = use_state(|| false);

    if *failed {
        return render_media_expired(props.filename.as_deref(), "video");
    }

    let on_error = {
        let failed = failed.clone();
        Callback::from(move |_: Event| failed.set(true))
    };

    // Use the `src` attribute directly (not a child `<source>`) so the media
    // element's own `error` event fires on a 404 — that's what drives the
    // "media expired" fallback when the bounded store has dropped the blob.
    html! {
        <div class="tool-result-video">
            <video
                controls=true
                preload="metadata"
                src={props.src.clone()}
                onerror={on_error}
            />
        </div>
    }
}

/// Dark-theme-friendly placeholder shown when a served media blob has been
/// evicted/expired from its bounded store (the transcript row outlives it).
fn render_media_expired(filename: Option<&str>, kind: &str) -> Html {
    let label = match filename {
        Some(name) => format!("media expired: {name}"),
        None => format!("{kind} expired"),
    };
    html! {
        <div class="media-expired">
            <span class="media-expired-icon">{ "\u{26a0}\u{fe0f}" }</span>
            <span class="media-expired-label">{ label }</span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_gets_a_size_fallback_but_rasters_do_not() {
        // SVG can lack intrinsic dimensions and collapse to 0x0 without it.
        assert!(needs_size_fallback("image/svg+xml"));
        // Raster formats always carry pixel dimensions; forcing a width on them
        // would stretch small images instead of letting the frame hug them.
        for raster in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            assert!(!needs_size_fallback(raster), "{raster} needs no fallback");
        }
    }

    #[test]
    fn every_allowed_image_type_is_classified() {
        // Guards against a new format being allowed without deciding whether it
        // can render without intrinsic dimensions.
        for media_type in ALLOWED_IMAGE_MEDIA_TYPES {
            let expected = *media_type == "image/svg+xml";
            assert_eq!(needs_size_fallback(media_type), expected, "{media_type}");
        }
    }
}
