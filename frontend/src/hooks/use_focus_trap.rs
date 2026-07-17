//! Focus management for modal dialogs (#1384).
//!
//! Keeps keyboard focus inside an open modal: moves focus into it on open and
//! cycles Tab / Shift+Tab through only the modal's own controls so focus never
//! escapes into the dashboard behind it.

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

/// Trap keyboard focus inside `container` while it is mounted.
///
/// - On mount, focuses the first focusable descendant. Both modals that use
///   this render their intended initial control first (the launch dialog's
///   first field; the confirm modal's Cancel button), so "focus the first
///   focusable" satisfies the acceptance criteria without extra wiring.
/// - Tab past the last control wraps to the first; Shift+Tab before the first
///   wraps to the last. If focus is somehow outside the set, Tab lands on the
///   first control and Shift+Tab on the last.
///
/// Returns an `onkeydown` handler to attach to the container element (pass the
/// same [`NodeRef`]). The handler only acts on Tab, leaving Enter/Escape and
/// typing to the component.
#[hook]
pub fn use_focus_trap(container: NodeRef) -> Callback<KeyboardEvent> {
    // Move focus into the modal once, after it mounts.
    {
        let container = container.clone();
        use_effect_with((), move |_| {
            if let Some(root) = container.cast::<Element>() {
                if let Some(first) = focusable_elements(&root).first() {
                    let _ = first.focus();
                }
            }
            || ()
        });
    }

    let container = container.clone();
    Callback::from(move |e: KeyboardEvent| {
        if e.key() != "Tab" {
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
        let active_in_container = active
            .as_ref()
            .map(|a| root.contains(Some(a.unchecked_ref::<Node>())))
            .unwrap_or(false);
        let is = |el: &HtmlElement| active.as_ref() == Some(el.unchecked_ref::<Element>());

        if e.shift_key() {
            // Shift+Tab off the first control (or focus outside the modal) → last.
            if !active_in_container || is(first) {
                e.prevent_default();
                let _ = last.focus();
            }
        } else if !active_in_container || is(last) {
            // Tab off the last control (or focus outside the modal) → first.
            e.prevent_default();
            let _ = first.focus();
        }
    })
}
