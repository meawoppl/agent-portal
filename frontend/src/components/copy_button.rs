//! Small reusable clipboard copy button.

use gloo::timers::callback::Timeout;
use wasm_bindgen_futures::spawn_local;
use web_sys::{window, MouseEvent};
use yew::prelude::*;

/// Write `text` to the clipboard, set `copied` to `true` once the write
/// completes, then reset it to `false` after `reset_ms` milliseconds.
pub fn copy_to_clipboard(text: String, copied: UseStateHandle<bool>, reset_ms: u32) {
    spawn_local(async move {
        if let Some(window) = window() {
            let clipboard = window.navigator().clipboard();
            let promise = clipboard.write_text(&text);
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
            copied.set(true);
            let reset = copied.clone();
            Timeout::new(reset_ms, move || reset.set(false)).forget();
        }
    });
}

#[derive(Properties, PartialEq)]
pub struct CopyButtonProps {
    pub text: AttrValue,
    #[prop_or_default]
    pub class: Classes,
    /// Tooltip shown when not yet clicked
    #[prop_or(AttrValue::from("Copy"))]
    pub title: AttrValue,
}

#[function_component(CopyButton)]
pub fn copy_button(props: &CopyButtonProps) -> Html {
    let copied = use_state(|| false);

    let on_click = {
        let text = props.text.clone();
        let copied = copied.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            e.prevent_default();
            copy_to_clipboard(text.to_string(), copied.clone(), 1500);
        })
    };

    let title = if *copied { "Copied!" } else { &*props.title };
    let class = classes!(
        "copy-button",
        if *copied { "copied" } else { "" },
        props.class.clone()
    );

    html! {
        <button type="button" {class} onclick={on_click} title={title.to_string()} aria-label={title.to_string()}>
            if *copied {
                { "\u{2713}" }
            } else {
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none"
                    stroke="currentColor" stroke-width="2"
                    stroke-linecap="round" stroke-linejoin="round">
                    <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                    <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                </svg>
            }
        </button>
    }
}
