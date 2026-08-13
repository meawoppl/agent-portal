//! Shared modal/overlay chrome (#1655).
//!
//! Every dialog in the app is the same two-element sandwich — a full-viewport
//! backdrop that closes on click, wrapping a pane that swallows clicks so they
//! don't reach it — plus Escape handling and a focus trap. Each one grew its
//! own copy, and they drifted:
//!
//! | Dialog | Escape | Focus trap |
//! |---|---|---|
//! | `confirm_modal` | capture-phase | yes |
//! | `launch_dialog` | capture-phase | yes |
//! | `help_overlay` | capture-phase | **no** |
//! | `schedule_dialog` | **bubble-phase** | **no** |
//! | `fork_dialog` | **none** | **no** |
//! | `share_dialog` | hand-rolled listener | **no** |
//!
//! So this is not only deduplication: three of the six could not be dismissed
//! with Escape at all or leaked Tab focus to the page behind them, and
//! `schedule_dialog`'s bubble-phase listener let Escape also reach the
//! keyboard-nav handler underneath and toggle nav mode on the way out.
//!
//! Escape is capture-phase for exactly that reason — see
//! [`use_escape_capture`](crate::hooks::use_escape_capture). The focus trap
//! also restores focus to the invoking element on close (#1384), so dismissing
//! a dialog doesn't strand focus on `<body>` and force a click back into the
//! composer.
//!
//! Callers keep their own class names: the chrome is behavioral, and the
//! per-dialog CSS (`help-overlay`, `sched-overlay`, `launch-dialog-backdrop`,
//! `modal-overlay`) is deliberately left alone so this change moves no pixels.

use yew::prelude::*;

use crate::hooks::{use_escape_capture, use_focus_trap};

#[derive(Properties, PartialEq)]
pub struct FloatingPaneProps {
    /// Class for the backdrop element, e.g. `modal-overlay`.
    pub overlay_class: AttrValue,
    /// Class for the pane itself, e.g. `help-dialog`.
    pub pane_class: AttrValue,
    /// Escape, backdrop click, or a child's own dismiss control.
    pub on_close: Callback<()>,
    #[prop_or_default]
    pub children: Html,
}

#[function_component(FloatingPane)]
pub fn floating_pane(props: &FloatingPaneProps) -> Html {
    let container = use_node_ref();
    use_focus_trap(container.clone());
    use_escape_capture(true, props.on_close.clone());

    let on_backdrop = {
        let on_close = props.on_close.clone();
        Callback::from(move |_: MouseEvent| on_close.emit(()))
    };

    html! {
        <div class={props.overlay_class.clone()} onclick={on_backdrop}>
            <div
                ref={container}
                class={props.pane_class.clone()}
                onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
            >
                { props.children.clone() }
            </div>
        </div>
    }
}
