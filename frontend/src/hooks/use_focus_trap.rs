//! Focus management for modal dialogs (#1384).
//!
//! Keeps keyboard focus inside an open modal: moves focus into it on open and
//! cycles Tab / Shift+Tab through only the modal's own controls so focus never
//! escapes into the dashboard behind it.

use gloo::events::{EventListener, EventListenerOptions, EventListenerPhase};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, Node};
use yew::prelude::*;

/// CSS selector matching the natively focusable controls a modal can contain.
/// `:not([disabled])` drops disabled inputs/buttons, and `[tabindex='-1']` is
/// excluded so programmatically-focusable-only nodes aren't part of the cycle.
const FOCUSABLE: &str = "a[href], button:not([disabled]), input:not([disabled]), \
     select:not([disabled]), textarea:not([disabled]), \
     [tabindex]:not([tabindex='-1'])";

/// Collect the focusable descendants of `root` in DOM order.
fn focusable_elements(root: &Element) -> Vec<HtmlElement> {
    let Ok(list) = root.query_selector_all(FOCUSABLE) else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<HtmlElement>().ok())
        .collect()
}

/// Trap keyboard focus inside `container` while it is mounted, and move focus
/// into it on open.
///
/// - On mount, focuses the first focusable descendant. Both modals that use
///   this render their intended initial control first (the launch dialog's
///   first field; the confirm modal's Cancel button), so "focus the first
///   focusable" satisfies the acceptance criteria without extra wiring.
/// - Tab past the last control wraps to the first; Shift+Tab before the first
///   wraps to the last. If focus has escaped the modal entirely (e.g. a
///   re-render swapped out the initially-focused node), the next Tab pulls it
///   back to the first control and Shift+Tab to the last.
///
/// The Tab handling is a **capture-phase document listener**, not a handler on
/// the container: a container-scoped `onkeydown` only fires while focus is
/// already inside it, so it can't recover focus once it has escaped to
/// `<body>`. Capturing at the document catches Tab wherever focus currently
/// sits. The listener lives for as long as the modal (this hook's component) is
/// mounted.
#[hook]
pub fn use_focus_trap(container: NodeRef) {
    // Move focus into the modal once, after it mounts, and restore it to the
    // pre-open element when the modal closes. Without the restore, closing the
    // modal (Cancel / Escape / backdrop) leaves focus on `<body>` — the user
    // has to click back into the composer to type again (reported on the launch
    // dialog cancel path, #1384).
    {
        let container = container.clone();
        use_effect_with((), move |_| {
            let previously_focused = gloo::utils::document()
                .active_element()
                .and_then(|el| el.dyn_into::<HtmlElement>().ok());
            if let Some(root) = container.cast::<Element>() {
                if let Some(first) = focusable_elements(&root).first() {
                    let _ = first.focus();
                }
            }
            move || {
                // Only restore if that element is still in the document (it may
                // have been removed — e.g. a confirm modal that deleted the
                // thing its trigger lived in). Refocusing a detached node would
                // throw / no-op, so guard on `is_connected`.
                if let Some(el) = previously_focused {
                    if el.is_connected() {
                        let _ = el.focus();
                    }
                }
            }
        });
    }

    // Trap Tab at the document (capture phase) for the modal's lifetime.
    use_effect_with((), move |_| {
        let options = EventListenerOptions {
            phase: EventListenerPhase::Capture,
            passive: false,
        };
        let listener = EventListener::new_with_options(
            &gloo::utils::document(),
            "keydown",
            options,
            move |event| {
                let Some(ke) = event.dyn_ref::<web_sys::KeyboardEvent>() else {
                    return;
                };
                if ke.key() != "Tab" {
                    return;
                }
                let Some(root) = container.cast::<Element>() else {
                    return;
                };
                let focusables = focusable_elements(&root);
                let (Some(first), Some(last)) = (focusables.first(), focusables.last()) else {
                    return;
                };

                let active = gloo::utils::document().active_element();
                let active_in_modal = active
                    .as_ref()
                    .map(|a| root.contains(Some(a.unchecked_ref::<Node>())))
                    .unwrap_or(false);
                let is = |el: &HtmlElement| active.as_ref() == Some(el.unchecked_ref::<Element>());

                if ke.shift_key() {
                    // Shift+Tab off the first control (or focus outside the modal) → last.
                    if !active_in_modal || is(first) {
                        ke.prevent_default();
                        let _ = last.focus();
                    }
                } else if !active_in_modal || is(last) {
                    // Tab off the last control (or focus outside the modal) → first.
                    ke.prevent_default();
                    let _ = first.focus();
                }
            },
        );
        move || drop(listener)
    });
}
