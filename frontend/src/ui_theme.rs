//! Whole-app visual theme (Settings > Appearance).
//!
//! A per-browser choice that sets `<html data-ui="…">`. The default ("linux")
//! sets no attribute and is the legacy styling; the others restyle the app
//! from stylesheets whose every rule is scoped to their attribute value
//! (`styles/refresh/` for "apple", `styles/themes/windows.css` for
//! "windows"). The inline boot script in `index.html` applies the same
//! attribute from the same storage key before first paint, so this module
//! only handles live switching.

use crate::utils::{storage_get, storage_set};

/// localStorage key; read by the boot script in `index.html` too.
pub const UI_THEME_STORAGE_KEY: &str = "claude-portal-ui-theme";

/// Web fonts the "apple" theme falls back to off Apple hardware, fetched only
/// once that theme is chosen.
const APPLE_FONT_STYLESHEETS: &[(&str, &str)] = &[
    (
        "ui-theme-font-sans",
        "https://cdn.jsdelivr.net/npm/@fontsource-variable/inter@5/index.css",
    ),
    (
        "ui-theme-font-mono",
        "https://cdn.jsdelivr.net/npm/@fontsource-variable/jetbrains-mono@5/index.css",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTheme {
    Linux,
    Windows,
    Apple,
}

impl UiTheme {
    pub const ALL: [UiTheme; 3] = [UiTheme::Linux, UiTheme::Windows, UiTheme::Apple];

    pub fn as_str(self) -> &'static str {
        match self {
            UiTheme::Linux => "linux",
            UiTheme::Windows => "windows",
            UiTheme::Apple => "apple",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.as_str() == value)
    }

    /// The selector card's headline.
    pub fn label(self) -> &'static str {
        match self {
            UiTheme::Linux => "I want it to look like a terminal and hurt a little",
            UiTheme::Windows => {
                "I haven't changed my desktop computer in 20 years, and I don't want to start today"
            }
            UiTheme::Apple => "I want to live in Steve Jobs fantasies",
        }
    }

    /// The selector card's small print.
    pub fn caption(self) -> &'static str {
        match self {
            UiTheme::Linux => "The classic Agent Portal look.",
            UiTheme::Windows => "Bevels, gradients, and a title bar you can trust.",
            UiTheme::Apple => "Liquid glass over a drifting wallpaper. Preview.",
        }
    }
}

pub fn load_ui_theme() -> UiTheme {
    storage_get(UI_THEME_STORAGE_KEY)
        .and_then(|value| UiTheme::parse(&value))
        .unwrap_or(UiTheme::Linux)
}

/// Persist the theme and apply it to the live document.
pub fn set_ui_theme(theme: UiTheme) {
    storage_set(UI_THEME_STORAGE_KEY, theme.as_str());
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(root) = document.document_element() else {
        return;
    };
    match theme {
        UiTheme::Linux => {
            let _ = root.remove_attribute("data-ui");
        }
        UiTheme::Windows => {
            let _ = root.set_attribute("data-ui", theme.as_str());
        }
        UiTheme::Apple => {
            let _ = root.set_attribute("data-ui", theme.as_str());
            ensure_apple_fonts(&document);
        }
    }
}

fn ensure_apple_fonts(document: &web_sys::Document) {
    let Some(head) = document.head() else {
        return;
    };
    for (id, href) in APPLE_FONT_STYLESHEETS {
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
