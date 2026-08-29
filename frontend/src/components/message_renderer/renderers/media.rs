//! Media renderers shared by the assistant and portal families: the image
//! lightbox, the `agent-portal show` video player, and the expired-blob
//! placeholder both degrade to.

use crate::hooks::use_escape_capture;
use gloo::events::EventListener;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlIFrameElement, HtmlInputElement};
use yew::prelude::*;

#[wasm_bindgen(module = "/rizzma-host.js")]
extern "C" {
    #[wasm_bindgen(js_name = mountRizzma)]
    fn mount_rizzma(
        frame: HtmlIFrameElement,
        artifact_url: &str,
        renderer_version: &str,
    ) -> js_sys::Promise;

    #[wasm_bindgen(js_name = playRizzma)]
    fn play_rizzma(frame: HtmlIFrameElement);

    #[wasm_bindgen(js_name = pauseRizzma)]
    fn pause_rizzma(frame: HtmlIFrameElement);

    #[wasm_bindgen(js_name = seekRizzma)]
    fn seek_rizzma(frame: HtmlIFrameElement, time: f64);

    #[wasm_bindgen(js_name = disposeRizzma)]
    fn dispose_rizzma(frame: HtmlIFrameElement);
}

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
pub(super) struct FigureViewerProps {
    pub artifact_url: String,
    pub width_px: u32,
    pub height_px: u32,
    #[prop_or_default]
    pub title: Option<String>,
    #[prop_or_default]
    pub alt: Option<String>,
    #[prop_or_default]
    pub poster_base64: Option<String>,
    pub animated: bool,
    pub duration: f64,
    pub renderer_version: String,
    pub live_supported: bool,
}

/// Only exact, host-vetted runtime versions may execute. Rizzma 1.9 supports
/// static interaction; 1.10 and 1.11 support seeking on an already-bound
/// session, which keeps animation controls compatible with pan and zoom.
pub(super) fn figure_live_supported(schema: u32, renderer_version: &str, animated: bool) -> bool {
    schema <= 3
        && match renderer_version {
            "1.9.0" => !animated,
            "1.10.0" | "1.11.0" => true,
            _ => false,
        }
}

/// Sandboxed Rizzma viewer. Runtime assets are pinned and verified by the
/// portal-owned host module before any realm is created. The iframe has only
/// `allow-scripts`; it receives bytes through a MessageChannel and has no
/// network, storage, cookie, or portal-origin access.
#[function_component(FigureViewer)]
pub(super) fn figure_viewer(props: &FigureViewerProps) -> Html {
    let frame_ref = use_node_ref();
    let loading = use_state(|| false);
    let mounted = use_state(|| false);
    let playing = use_state(|| false);
    let position = use_state(|| 0.0_f64);
    let error = use_state(|| None::<String>);

    {
        let frame_ref = frame_ref.clone();
        use_effect_with((), move |_| {
            move || {
                if let Some(frame) = frame_ref.cast::<HtmlIFrameElement>() {
                    dispose_rizzma(frame);
                }
            }
        });
    }

    {
        let frame_ref = frame_ref.clone();
        let playing = playing.clone();
        let position = position.clone();
        use_effect_with(*mounted, move |is_mounted| {
            let listener = if *is_mounted {
                frame_ref.cast::<HtmlIFrameElement>().map(|frame| {
                    let state_frame = frame.clone();
                    EventListener::new(&frame, "rizzma-state", move |_| {
                        playing.set(
                            state_frame.get_attribute("data-rizzma-playing").as_deref()
                                == Some("true"),
                        );
                        if let Some(value) = state_frame
                            .get_attribute("data-rizzma-time")
                            .and_then(|value| value.parse::<f64>().ok())
                            .filter(|value| value.is_finite())
                        {
                            position.set(value);
                        }
                    })
                })
            } else {
                None
            };
            move || drop(listener)
        });
    }

    let onclick = {
        let frame_ref = frame_ref.clone();
        let artifact_url = props.artifact_url.clone();
        let renderer_version = props.renderer_version.clone();
        let loading = loading.clone();
        let mounted = mounted.clone();
        let error = error.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(frame) = frame_ref.cast::<HtmlIFrameElement>() else {
                error.set(Some("portable-figure frame is unavailable".to_string()));
                return;
            };
            loading.set(true);
            error.set(None);
            let loading = loading.clone();
            let mounted = mounted.clone();
            let error = error.clone();
            let promise = mount_rizzma(frame, &artifact_url, &renderer_version);
            wasm_bindgen_futures::spawn_local(async move {
                match JsFuture::from(promise).await {
                    Ok(_) => mounted.set(true),
                    Err(value) => error.set(Some(
                        value
                            .as_string()
                            .unwrap_or_else(|| "portable figure failed to mount".to_string()),
                    )),
                }
                loading.set(false);
            });
        })
    };

    let on_play_pause = {
        let frame_ref = frame_ref.clone();
        let playing = playing.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(frame) = frame_ref.cast::<HtmlIFrameElement>() else {
                return;
            };
            if *playing {
                pause_rizzma(frame);
            } else {
                play_rizzma(frame);
            }
        })
    };

    let on_seek = {
        let frame_ref = frame_ref.clone();
        Callback::from(move |event: InputEvent| {
            let value = event
                .target_unchecked_into::<HtmlInputElement>()
                .value_as_number();
            if !value.is_finite() {
                return;
            }
            if let Some(frame) = frame_ref.cast::<HtmlIFrameElement>() {
                seek_rizzma(frame, value);
            }
        })
    };

    let aspect_ratio = format!("{} / {}", props.width_px.max(1), props.height_px.max(1));
    let poster = props
        .poster_base64
        .as_ref()
        .map(|data| format!("data:image/png;base64,{data}"));
    let label = props
        .alt
        .clone()
        .or_else(|| props.title.clone())
        .unwrap_or_else(|| "Portable figure".to_string());

    html! {
        <div class="rizzma-figure">
            <div class="rizzma-viewport" style={format!("aspect-ratio: {aspect_ratio}")}>
                if !*mounted {
                    if let Some(src) = poster {
                        <img class="rizzma-poster" {src} alt={label.clone()} />
                    } else {
                        <div class="rizzma-poster-missing">{ label.clone() }</div>
                    }
                }
                <iframe
                    ref={frame_ref}
                    class={classes!("rizzma-frame", (!*mounted).then_some("hidden"))}
                    sandbox="allow-scripts"
                    title={label}
                />
                if !*mounted {
                    <button class="rizzma-mount" {onclick} disabled={*loading || !props.live_supported}>
                        if !props.live_supported {
                            { "Poster (runtime unavailable)" }
                        } else if *loading {
                            { "Loading interactive figure…" }
                        } else if props.animated {
                            { "Play interactive figure" }
                        } else {
                            { "Open interactive figure" }
                        }
                    </button>
                }
                if let Some(message) = &*error {
                    <div class="rizzma-error">{ message }</div>
                }
            </div>
            if *mounted && props.animated {
                <div class="rizzma-controls">
                    <button type="button" onclick={on_play_pause}>
                        { if *playing { "Pause" } else { "Play" } }
                    </button>
                    <input
                        type="range"
                        min="0"
                        max={props.duration.max(0.0).to_string()}
                        step="0.01"
                        value={(*position).to_string()}
                        oninput={on_seek}
                        aria-label="Animation position"
                    />
                    <span>{ format!("{:.1}s / {:.1}s", *position, props.duration.max(0.0)) }</span>
                </div>
            }
        </div>
    }
}

#[cfg(test)]
mod figure_tests {
    use super::figure_live_supported;

    #[test]
    fn runtime_capability_distinguishes_static_and_animated_figures() {
        assert!(figure_live_supported(3, "1.9.0", false));
        assert!(!figure_live_supported(3, "1.9.0", true));
        assert!(figure_live_supported(3, "1.10.0", false));
        assert!(figure_live_supported(3, "1.10.0", true));
        assert!(figure_live_supported(3, "1.11.0", false));
        assert!(figure_live_supported(3, "1.11.0", true));
        assert!(!figure_live_supported(4, "1.10.0", false));
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
