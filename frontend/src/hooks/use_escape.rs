//! Hooks for closing overlays when the Escape key is pressed.

use gloo::events::{EventListener, EventListenerOptions, EventListenerPhase};
use wasm_bindgen::JsCast;
use yew::prelude::*;

/// Build a document-level `keydown` listener (bubble phase) that emits
/// `on_close` when Escape is pressed.
///
/// For struct components that cannot use hooks: hold the returned listener
/// (RAII guard) for as long as the handler should stay active. Function
/// components should use [`use_escape`] instead.
pub fn escape_listener(on_close: Callback<()>) -> EventListener {
    EventListener::new(&gloo::utils::document(), "keydown", move |event| {
        if let Some(ke) = event.dyn_ref::<web_sys::KeyboardEvent>() {
            if ke.key() == "Escape" {
                on_close.emit(());
            }
        }
    })
}

/// Emit `on_close` when Escape is pressed, attached only while `active`.
///
/// Capture-phase, and consumes the press (`prevent_default` +
/// `stop_propagation`) before emitting `on_close`, so it never reaches
/// bubble-phase handlers such as the keyboard-nav mode toggle.
///
/// A bubble-phase `use_escape` used to exist alongside this. It was removed
/// with #1655: its only caller was the schedule dialog, where Escape closed
/// the dialog *and* went on to toggle nav mode underneath. Every dialog now
/// gets this variant via [`FloatingPane`](crate::components::FloatingPane),
/// so there is no reason to offer the leaky one.
#[hook]
pub fn use_escape_capture(active: bool, on_close: Callback<()>) {
    use_effect_with(active, move |is_active| {
        let listener = is_active.then(|| {
            let options = EventListenerOptions {
                phase: EventListenerPhase::Capture,
                passive: false,
            };
            EventListener::new_with_options(
                &gloo::utils::document(),
                "keydown",
                options,
                move |event| {
                    if let Some(ke) = event.dyn_ref::<web_sys::KeyboardEvent>() {
                        if ke.key() == "Escape" {
                            ke.prevent_default();
                            ke.stop_propagation();
                            on_close.emit(());
                        }
                    }
                },
            )
        });
        move || drop(listener)
    });
}
