//! Keyboard shortcuts help overlay (issue #1324).
//!
//! A modal listing every keyboard shortcut, grouped by context. Opened by
//! pressing `?` on the dashboard while not typing in the message textarea, and
//! dismissed with `Esc` (capture-phase, so it never toggles nav mode) or a
//! backdrop click.

use crate::components::FloatingPane;
use crate::pages::dashboard::load_nav_mode_shortcut;
use web_sys::MouseEvent;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct HelpOverlayProps {
    pub on_close: Callback<()>,
}

/// A single shortcut row: one or more key labels and what they do.
struct Shortcut {
    keys: Keys,
    description: &'static str,
}

/// Key labels for a row: fixed, or the user's configured Nav mode chord.
enum Keys {
    Fixed(&'static [&'static str]),
    NavModeToggle,
}

/// A named group of shortcuts (a "context" such as edit mode or nav mode).
struct ShortcutGroup {
    title: &'static str,
    shortcuts: &'static [Shortcut],
}

const GROUPS: &[ShortcutGroup] = &[
    ShortcutGroup {
        title: "Global",
        shortcuts: &[
            Shortcut {
                keys: Keys::Fixed(&["?"]),
                description: "Show this shortcuts overlay",
            },
            Shortcut {
                keys: Keys::Fixed(&["Shift", "Tab"]),
                description: "Jump to the next active session",
            },
            Shortcut {
                keys: Keys::NavModeToggle,
                description: "Toggle nav mode (enter / leave)",
            },
            Shortcut {
                keys: Keys::Fixed(&["Ctrl", "Z"]),
                description: "Interrupt the running agent",
            },
        ],
    },
    ShortcutGroup {
        title: "Edit mode (typing)",
        shortcuts: &[
            Shortcut {
                keys: Keys::Fixed(&["Enter"]),
                description: "Send message",
            },
            Shortcut {
                keys: Keys::Fixed(&["Shift", "Enter"]),
                description: "Insert a new line",
            },
            Shortcut {
                keys: Keys::Fixed(&["↑"]),
                description: "Previous message in input history",
            },
            Shortcut {
                keys: Keys::Fixed(&["↓"]),
                description: "Next message in input history",
            },
            Shortcut {
                keys: Keys::Fixed(&["Ctrl", "M"]),
                description: "Toggle voice input",
            },
        ],
    },
    ShortcutGroup {
        title: "Nav mode",
        shortcuts: &[
            Shortcut {
                keys: Keys::Fixed(&["↑", "↓", "←", "→"]),
                description: "Move between sessions",
            },
            Shortcut {
                keys: Keys::Fixed(&["h", "l"]),
                description: "Move between sessions (vim keys)",
            },
            Shortcut {
                keys: Keys::Fixed(&["j", "k"]),
                description: "Scroll the focused transcript down / up",
            },
            Shortcut {
                keys: Keys::Fixed(&["g", "g"]),
                description: "Jump to the top of the transcript",
            },
            Shortcut {
                keys: Keys::Fixed(&["1", "–", "9"]),
                description: "Jump directly to a session by number",
            },
            Shortcut {
                keys: Keys::Fixed(&["w"]),
                description: "Jump to the next waiting session",
            },
            Shortcut {
                keys: Keys::Fixed(&["G"]),
                description: "Jump to the latest message (resume live tailing)",
            },
            Shortcut {
                keys: Keys::Fixed(&["n"]),
                description: "New session (open the launch dialog)",
            },
            Shortcut {
                keys: Keys::Fixed(&["d"]),
                description: "Delete the focused session (with confirmation)",
            },
            Shortcut {
                keys: Keys::Fixed(&["x"]),
                description: "Interrupt the focused session's agent",
            },
            Shortcut {
                keys: Keys::Fixed(&["c"]),
                description: "Collapse (hide) / expand the focused session",
            },
            Shortcut {
                keys: Keys::Fixed(&["Enter"]),
                description: "Accept the current pane and return to edit mode",
            },
            Shortcut {
                keys: Keys::NavModeToggle,
                description: "Return to edit mode",
            },
        ],
    },
];

#[function_component(HelpOverlay)]
pub fn help_overlay(props: &HelpOverlayProps) -> Html {
    // Escape (capture-phase, so it never toggles nav mode), the backdrop
    // click, and the focus trap all come from `FloatingPane` (#1655).
    let on_close_button = {
        let on_close = props.on_close.clone();
        Callback::from(move |_: MouseEvent| on_close.emit(()))
    };
    let nav_mode_keys = load_nav_mode_shortcut().keys();

    html! {
        <FloatingPane
            overlay_class="help-overlay"
            pane_class="help-dialog"
            on_close={props.on_close.clone()}
        >
                <div class="help-dialog-header">
                    <h2>{ "Keyboard Shortcuts" }</h2>
                    <button
                        class="help-dialog-close"
                        onclick={on_close_button}
                        aria-label="Close"
                    >
                        { "×" }
                    </button>
                </div>
                <div class="help-dialog-body">
                    {
                        GROUPS.iter().map(|group| html! {
                            <section class="help-group" key={group.title}>
                                <h3 class="help-group-title">{ group.title }</h3>
                                <ul class="help-shortcut-list">
                                    {
                                        group.shortcuts.iter().map(|shortcut| html! {
                                            <li class="help-shortcut">
                                                <span class="help-keys">
                                                    {
                                                        match shortcut.keys {
                                                            Keys::Fixed(keys) => keys,
                                                            Keys::NavModeToggle => nav_mode_keys,
                                                        }.iter().map(|key| html! {
                                                            <kbd>{ *key }</kbd>
                                                        }).collect::<Html>()
                                                    }
                                                </span>
                                                <span class="help-description">
                                                    { shortcut.description }
                                                </span>
                                            </li>
                                        }).collect::<Html>()
                                    }
                                </ul>
                            </section>
                        }).collect::<Html>()
                    }
                </div>
                <div class="help-dialog-footer">
                    <span>{ "Press " }<kbd>{ "Esc" }</kbd>{ " or click outside to close" }</span>
                </div>
        </FloatingPane>
    }
}
