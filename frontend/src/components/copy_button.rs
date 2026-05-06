//! Small reusable clipboard copy button.

use gloo::timers::callback::Timeout;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{window, MouseEvent};
use yew::prelude::*;

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
            let text = text.to_string();
            let copied = copied.clone();
            spawn_local(async move {
                if let Some(window) = window() {
                    let navigator = window.navigator();
                    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into())
                        .ok()
                        .and_then(|v| v.dyn_into::<web_sys::Clipboard>().ok());
                    if let Some(clipboard) = clipboard {
                        let promise = clipboard.write_text(&text);
                        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                        copied.set(true);
                        let reset = copied.clone();
                        Timeout::new(1500, move || reset.set(false)).forget();
                    }
                }
            });
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
                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
                    <rect x="4" y="4" width="9" height="10" rx="1.2" />
                    <path d="M4 4V3a1 1 0 0 1 1-1h7a1 1 0 0 1 1 1v8" />
                </svg>
            }
        </button>
    }
}
