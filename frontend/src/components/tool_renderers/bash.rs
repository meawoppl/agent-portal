use crate::components::markdown::linkify_urls;
use crate::components::message_renderer::format_duration;
use crate::components::tool_renderers::extract_tool_input;
use serde_json::Value;
use shared::BashInput;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
struct BashToolProps {
    command: AttrValue,
    description: Option<AttrValue>,
    timeout_str: Option<AttrValue>,
    background: bool,
}

#[function_component(BashTool)]
fn bash_tool(props: &BashToolProps) -> Html {
    let expanded = use_state(|| false);
    let command = &*props.command;

    let toggle = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| {
            expanded.set(!*expanded);
        })
    };

    let cmd_class = if *expanded {
        "bash-command-inline expanded"
    } else {
        "bash-command-inline"
    };

    html! {
        <div class="tool-use bash-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "$" }</span>
                <span class="tool-name">{ "Bash" }</span>
                <code class={cmd_class} onclick={toggle} title="Click to expand">
                    { linkify_urls(command) }
                </code>
                <span class="tool-header-spacer"></span>
                {
                    if props.background {
                        html! { <span class="tool-badge background">{ "background" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(t) = &props.timeout_str {
                        html! { <span class="tool-meta timeout">{ format!("timeout={}", t) }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            {
                if let Some(desc) = &props.description {
                    html! { <div class="bash-description">{ desc }</div> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

pub fn render_bash_tool(input: &Value) -> Html {
    let bash = extract_tool_input::<BashInput>(input).unwrap_or(BashInput {
        command: String::new(),
        description: None,
        timeout: None,
        run_in_background: None,
    });

    let timeout_str = bash.timeout.map(format_duration);
    let background = bash.run_in_background.unwrap_or(false);

    html! {
        <BashTool
            command={bash.command}
            description={bash.description.map(AttrValue::from)}
            timeout_str={timeout_str.map(AttrValue::from)}
            {background}
        />
    }
}
