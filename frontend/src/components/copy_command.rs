//! Copy Command Component
//!
//! A styled code block with a copy-to-clipboard button.

use yew::prelude::*;

use crate::components::copy_button::copy_to_clipboard;

#[derive(Properties, PartialEq, Clone)]
pub struct CopyCommandProps {
    /// The command text to display and copy
    pub command: String,
    /// Optional label above the command
    #[prop_or_default]
    pub label: Option<String>,
}

#[function_component(CopyCommand)]
pub fn copy_command(props: &CopyCommandProps) -> Html {
    let copied = use_state(|| false);

    let on_copy = {
        let command = props.command.clone();
        let copied = copied.clone();

        Callback::from(move |_: MouseEvent| {
            copy_to_clipboard(command.clone(), copied.clone(), 2000);
        })
    };

    let button_class = if *copied {
        "copy-button copied"
    } else {
        "copy-button"
    };

    let button_text = if *copied { "Copied!" } else { "" };

    html! {
        <div class="copy-command-container">
            if let Some(label) = &props.label {
                <div class="copy-command-label">{ label }</div>
            }
            <div class="copy-command-block">
                <pre class="copy-command-text">{ &props.command }</pre>
                <button
                    class={button_class}
                    onclick={on_copy}
                    title="Copy to clipboard"
                >
                    <span class="copy-icon">
                        // Clipboard SVG icon
                        <svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                            <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
                            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
                        </svg>
                    </span>
                    <span class="copy-text">{ button_text }</span>
                </button>
            </div>
        </div>
    }
}
