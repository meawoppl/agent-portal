//! Keyboard shortcuts help overlay (issue #1324).
//!
//! A modal listing every keyboard shortcut, grouped by context. Opened by
//! pressing `?` on the dashboard while not typing in the message textarea, and
//! dismissed with `Esc` (capture-phase, so it never toggles nav mode) or a
//! backdrop click.

use crate::hooks::use_escape_capture;
use web_sys::MouseEvent;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct HelpOverlayProps {
    pub on_close: Callback<()>,
}

/// A single shortcut row: one or more key labels and what they do.
struct Shortcut {
    keys: &'static [&'static str],
    description: &'static str,
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
                keys: &["?"],
                description: "Show this shortcuts overlay",
            },
            Shortcut {
                keys: &["Shift", "Tab"],
                description: "Jump to the next active session",
            },
            Shortcut {
                keys: &["Ctrl/Cmd", "K"],
                description: "Toggle nav mode (enter / leave)",
            },
            Shortcut {
                keys: &["Ctrl", "C"],
                description: "Interrupt the running agent (copies if text is selected)",
            },
        ],
    },
    ShortcutGroup {
        title: "Edit mode (typing)",
        shortcuts: &[
            Shortcut {
                keys: &["Enter"],
                description: "Send message",
            },
            Shortcut {
                keys: &["Shift", "Enter"],
                description: "Insert a new line",
            },
            Shortcut {
                keys: &["↑"],
                description: "Previous message in input history",
            },
            Shortcut {
                keys: &["↓"],
                description: "Next message in input history",
            },
            Shortcut {
                keys: &["Ctrl", "M"],
                description: "Toggle voice input",
            },
        ],
    },
    ShortcutGroup {
        title: "Nav mode",
        shortcuts: &[
            Shortcut {
                keys: &["↑", "↓", "←", "→"],
                description: "Move between sessions",
            },
            Shortcut {
                keys: &["h", "j", "k", "l"],
                description: "Move between sessions (vim keys)",
            },
            Shortcut {
                keys: &["1", "–", "9"],
                description: "Jump directly to a session by number",
            },
            Shortcut {
                keys: &["w"],
                description: "Jump to the next waiting session",
            },
            Shortcut {
                keys: &["G"],
                description: "Jump to the latest message (resume live tailing)",
            },
            Shortcut {
                keys: &["n"],
                description: "New session (open the launch dialog)",
            },
            Shortcut {
                keys: &["d"],
                description: "Delete the focused session (with confirmation)",
            },
            Shortcut {
                keys: &["Enter"],
                description: "Accept the current pane and return to edit mode",
            },
            Shortcut {
                keys: &["Ctrl/Cmd", "K"],
                description: "Return to edit mode",
            },
        ],
    },
];

#[function_component(HelpOverlay)]
pub fn help_overlay(props: &HelpOverlayProps) -> Html {
    // Capture-phase Escape so it closes the overlay without reaching the
    // bubble-phase keyboard-nav handler (which would toggle nav mode).
    use_escape_capture(true, props.on_close.clone());

    let on_backdrop = {
        let on_close = props.on_close.clone();
        Callback::from(move |_: MouseEvent| on_close.emit(()))
    };

    let on_close_button = {
        let on_close = props.on_close.clone();
        Callback::from(move |_: MouseEvent| on_close.emit(()))
    };

    // Stop clicks inside the panel from bubbling to the backdrop (which closes).
    let stop = Callback::from(|e: MouseEvent| e.stop_propagation());

    html! {
        <div class="help-overlay" onclick={on_backdrop}>
            <div class="help-dialog" onclick={stop}>
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
                                                        shortcut.keys.iter().map(|key| html! {
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
            </div>
        </div>
    }
}
