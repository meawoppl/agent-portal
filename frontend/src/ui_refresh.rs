//! "Refreshed design (preview)" feature flag.
//!
//! A per-browser opt-in (Settings > Appearance) that sets
//! `<html data-ui="refresh">`; every rule in `styles/refresh/` is scoped to
//! that attribute, so with the flag off the legacy styling is untouched. The
//! inline boot script in `index.html` applies the same attribute from the
//! same storage key before first paint, which is why this module only needs
//! to handle live toggling.

use crate::utils::{storage_get, storage_set};

/// localStorage key; read by the boot script in `index.html` too.
pub const UI_REFRESH_STORAGE_KEY: &str = "claude-portal-ui-refresh";

/// Web fonts the refreshed design uses, fetched only once the flag is on.
const FONT_STYLESHEETS: &[(&str, &str)] = &[
    (
        "ui-refresh-font-sans",
        "https://cdn.jsdelivr.net/npm/@fontsource-variable/inter@5/index.css",
    ),
    (
        "ui-refresh-font-mono",
        "https://cdn.jsdelivr.net/npm/@fontsource-variable/jetbrains-mono@5/index.css",
    ),
];

pub fn load_ui_refresh() -> bool {
    storage_get(UI_REFRESH_STORAGE_KEY).is_some_and(|v| v == "true")
}

/// Persist the flag and apply it to the live document.
pub fn set_ui_refresh(enabled: bool) {
    storage_set(UI_REFRESH_STORAGE_KEY, if enabled { "true" } else { "false" });
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(root) = document.document_element() else {
        return;
    };
    if enabled {
        let _ = root.set_attribute("data-ui", "refresh");
        ensure_fonts(&document);
    } else {
        let _ = root.remove_attribute("data-ui");
    }
}

fn ensure_fonts(document: &web_sys::Document) {
    let Some(head) = document.head() else {
        return;
    };
    for (id, href) in FONT_STYLESHEETS {
        if document.get_element_by_id(id).is_some() {
            continue;
        }
        if let Ok(link) = document.create_element("link") {
            let _ = link.set_attribute("id", id);
            let _ = link.set_attribute("rel", "stylesheet");
            let _ = link.set_attribute("href", href);
            let _ = head.append_child(&link);
        }
    }
}
