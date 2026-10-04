//! Media renderers shared by the assistant and portal families: the image
//! lightbox, the `agent-portal show` video player, and the expired-blob
//! placeholder both degrade to.

use super::lightbox_gesture::{LightboxGesture, LightboxView, Point};
use crate::components::DismissibleBackdrop;
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

/// A mouse/pointer event's position as a viewport point.
fn client_point(event: &MouseEvent) -> Point {
    Point::new(f64::from(event.client_x()), f64::from(event.client_y()))
}

/// The viewport center the lightbox image is laid out around.
fn viewport_center() -> Point {
    let dimension = |value: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
        value.ok().and_then(|v| v.as_f64()).unwrap_or(0.0)
    };
    web_sys::window().map_or(Point::new(0.0, 0.0), |window| {
        Point::new(
            dimension(window.inner_width()) / 2.0,
            dimension(window.inner_height()) / 2.0,
        )
    })
}

#[function_component(ImageViewer)]
fn image_viewer(props: &ImageViewerProps) -> Html {
    let expanded = use_state(|| false);
    // The gesture (pointers, pinch baseline, current view) lives in a mut_ref,
    // not state: two fingers moving in one frame must each see the other's
    // update. `lightbox_view`/`dragging` mirror it for rendering.
    let gesture = use_mut_ref(LightboxGesture::default);
    let lightbox_view = use_state(LightboxView::default);
    let dragging = use_state(|| false);
    let sync_gesture = {
        let gesture = gesture.clone();
        let lightbox_view = lightbox_view.clone();
        let dragging = dragging.clone();
        Callback::from(move |()| {
            let gesture = gesture.borrow();
            lightbox_view.set(gesture.view());
            dragging.set(gesture.is_active());
        })
    };
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
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |_: MouseEvent| {
            gesture.borrow_mut().reset();
            sync_gesture.emit(());
            expanded.set(true);
        })
    };

    let close_lightbox = {
        let expanded = expanded.clone();
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |()| {
            gesture.borrow_mut().reset();
            sync_gesture.emit(());
            expanded.set(false);
        })
    };

    let reset_lightbox = {
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |_: MouseEvent| {
            gesture.borrow_mut().reset_view();
            sync_gesture.emit(());
        })
    };

    let on_lightbox_wheel = {
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |event: WheelEvent| {
            event.prevent_default();
            event.stop_propagation();
            // Mouse wheels report lines or pages on some browsers; the gesture
            // math wants pixels. 33px per line matches Firefox's own mapping.
            let delta = match event.delta_mode() {
                WheelEvent::DOM_DELTA_LINE => event.delta_y() * 33.0,
                WheelEvent::DOM_DELTA_PAGE => event.delta_y() * 400.0,
                _ => event.delta_y(),
            };
            gesture.borrow_mut().wheel(
                client_point(&event),
                delta,
                event.ctrl_key(),
                viewport_center(),
            );
            sync_gesture.emit(());
        })
    };

    let on_lightbox_pointer_down = {
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |event: PointerEvent| {
            // A mouse drags with the primary button only; touch and pen report
            // button 0 on contact.
            if event.button() != 0 {
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            gesture
                .borrow_mut()
                .pointer_down(event.pointer_id(), client_point(&event));
            sync_gesture.emit(());
        })
    };

    let on_lightbox_pointer_move = {
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |event: PointerEvent| {
            let tracked = gesture.borrow_mut().pointer_move(
                event.pointer_id(),
                client_point(&event),
                viewport_center(),
            );
            if tracked {
                event.prevent_default();
                event.stop_propagation();
                sync_gesture.emit(());
            }
        })
    };

    let on_lightbox_pointer_end = {
        let gesture = gesture.clone();
        let sync_gesture = sync_gesture.clone();
        Callback::from(move |event: PointerEvent| {
            event.prevent_default();
            event.stop_propagation();
            gesture.borrow_mut().pointer_up(event.pointer_id());
            sync_gesture.emit(());
        })
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
                <DismissibleBackdrop class="image-lightbox" on_close={close_lightbox.clone()}>
                    <div
                        class={classes!("image-lightbox-content", dragging.then_some("dragging"))}
                        onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
                        onwheel={on_lightbox_wheel}
                        onpointerdown={on_lightbox_pointer_down}
                        onpointermove={on_lightbox_pointer_move}
                        onpointerup={on_lightbox_pointer_end.clone()}
                        onpointercancel={on_lightbox_pointer_end}
                        ondblclick={reset_lightbox.clone()}
                    >
                        <img
                            class={classes!(size_fallback)}
                            style={lightbox_view.style()}
                            src={props.src.clone()}
                            alt="Full size image"
                            draggable="false"
                        />
                        <div
                            class="image-lightbox-controls"
                            onpointerdown={Callback::from(|event: PointerEvent| event.stop_propagation())}
                            onwheel={Callback::from(|event: WheelEvent| event.stop_propagation())}
                        >
                            <button class="image-lightbox-reset" onclick={reset_lightbox}>
                                { "Reset" }
                            </button>
                            <a
                                class="image-lightbox-download"
                                href={props.src.clone()}
                                download={download_name}
                            >
                                { "Download" }
                            </a>
                            <button class="image-lightbox-close" onclick={close_lightbox.reform(|_: MouseEvent| ())}>
                                { "\u{00d7}" }
                            </button>
                        </div>
                    </div>
                </DismissibleBackdrop>
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
pub(super) struct LegacyFigureViewerProps {
    pub width_px: u32,
    pub height_px: u32,
    #[prop_or_default]
    pub title: Option<String>,
    #[prop_or_default]
    pub alt: Option<String>,
    #[prop_or_default]
    pub poster_base64: Option<String>,
}

/// Legacy portable figures are poster-only. The portal no longer ships a local
/// interactive runtime; active figures should be hosted as a web surface and
/// exposed through the forwarding proxy.
#[function_component(LegacyFigureViewer)]
pub(super) fn legacy_figure_viewer(props: &LegacyFigureViewerProps) -> Html {
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
        <div class="portable-figure">
            <div class="portable-figure-viewport" style={format!("aspect-ratio: {aspect_ratio}")}>
                if let Some(src) = poster {
                    <img class="portable-figure-poster" {src} alt={label.clone()} />
                } else {
                    <div class="portable-figure-poster-missing">{ label.clone() }</div>
                }
            </div>
            <div class="portable-figure-note">
                { "Interactive portable figures are no longer rendered inside Portal. Host the animation as a site and open it through the forwarding proxy." }
            </div>
        </div>
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
