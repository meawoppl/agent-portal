//! Media renderers shared by the assistant and portal families: the image
//! lightbox, the `agent-portal show` video player, and the expired-blob
//! placeholder both degrade to.

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

const LIGHTBOX_MIN_SCALE: f64 = 0.5;
const LIGHTBOX_MAX_SCALE: f64 = 12.0;

#[derive(Clone, Copy, PartialEq)]
struct ImageLightboxView {
    scale: f64,
    x: f64,
    y: f64,
}

impl Default for ImageLightboxView {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        }
    }
}

impl ImageLightboxView {
    fn constrain(self) -> Self {
        let scale = self.scale.clamp(LIGHTBOX_MIN_SCALE, LIGHTBOX_MAX_SCALE);
        if scale <= 1.0 {
            return Self::default();
        }
        Self {
            scale,
            x: self.x,
            y: self.y,
        }
    }

    fn transform(self) -> String {
        format!(
            "translate({:.1}px, {:.1}px) scale({:.4})",
            self.x, self.y, self.scale
        )
    }
}

#[derive(Clone, Copy, PartialEq)]
struct ImageLightboxDrag {
    pointer_id: i32,
    start_client_x: f64,
    start_client_y: f64,
    start_view: ImageLightboxView,
}

#[function_component(ImageViewer)]
fn image_viewer(props: &ImageViewerProps) -> Html {
    let expanded = use_state(|| false);
    let lightbox_view = use_state(ImageLightboxView::default);
    let lightbox_drag = use_state(|| None::<ImageLightboxDrag>);
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
        let lightbox_view = lightbox_view.clone();
        let lightbox_drag = lightbox_drag.clone();
        Callback::from(move |_: MouseEvent| {
            lightbox_view.set(ImageLightboxView::default());
            lightbox_drag.set(None);
            expanded.set(true);
        })
    };

    let close_lightbox = {
        let expanded = expanded.clone();
        let lightbox_drag = lightbox_drag.clone();
        Callback::from(move |()| {
            lightbox_drag.set(None);
            expanded.set(false);
        })
    };

    let reset_lightbox = {
        let lightbox_view = lightbox_view.clone();
        let lightbox_drag = lightbox_drag.clone();
        Callback::from(move |_: MouseEvent| {
            lightbox_drag.set(None);
            lightbox_view.set(ImageLightboxView::default());
        })
    };

    let on_lightbox_wheel = {
        let lightbox_view = lightbox_view.clone();
        Callback::from(move |event: WheelEvent| {
            event.prevent_default();
            event.stop_propagation();

            let current = *lightbox_view;
            let direction = if event.delta_y() < 0.0 { 1.0 } else { -1.0 };
            let multiplier = if direction > 0.0 { 1.18 } else { 1.0 / 1.18 };
            let next_scale =
                (current.scale * multiplier).clamp(LIGHTBOX_MIN_SCALE, LIGHTBOX_MAX_SCALE);

            let Some(window) = web_sys::window() else {
                lightbox_view.set(
                    ImageLightboxView {
                        scale: next_scale,
                        ..current
                    }
                    .constrain(),
                );
                return;
            };
            let viewport_width = window
                .inner_width()
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            let viewport_height = window
                .inner_height()
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            let focus_x = event.client_x() as f64 - viewport_width / 2.0;
            let focus_y = event.client_y() as f64 - viewport_height / 2.0;
            let ratio = next_scale / current.scale.max(0.001);
            lightbox_view.set(
                ImageLightboxView {
                    scale: next_scale,
                    x: focus_x - (focus_x - current.x) * ratio,
                    y: focus_y - (focus_y - current.y) * ratio,
                }
                .constrain(),
            );
        })
    };

    let on_lightbox_pointer_down = {
        let lightbox_view = lightbox_view.clone();
        let lightbox_drag = lightbox_drag.clone();
        Callback::from(move |event: PointerEvent| {
            if event.button() != 0 {
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            lightbox_drag.set(Some(ImageLightboxDrag {
                pointer_id: event.pointer_id(),
                start_client_x: event.client_x() as f64,
                start_client_y: event.client_y() as f64,
                start_view: *lightbox_view,
            }));
        })
    };

    let on_lightbox_pointer_move = {
        let lightbox_view = lightbox_view.clone();
        let lightbox_drag = lightbox_drag.clone();
        Callback::from(move |event: PointerEvent| {
            let Some(drag) = *lightbox_drag else {
                return;
            };
            if drag.pointer_id != event.pointer_id() {
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            lightbox_view.set(
                ImageLightboxView {
                    x: drag.start_view.x + event.client_x() as f64 - drag.start_client_x,
                    y: drag.start_view.y + event.client_y() as f64 - drag.start_client_y,
                    ..drag.start_view
                }
                .constrain(),
            );
        })
    };

    let on_lightbox_pointer_end = {
        let lightbox_drag = lightbox_drag.clone();
        Callback::from(move |event: PointerEvent| {
            let should_clear = lightbox_drag
                .as_ref()
                .is_some_and(|drag| drag.pointer_id == event.pointer_id());
            if should_clear {
                event.prevent_default();
                event.stop_propagation();
                lightbox_drag.set(None);
            }
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
                        class={classes!("image-lightbox-content", lightbox_drag.is_some().then_some("dragging"))}
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
                            style={lightbox_view.transform()}
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
