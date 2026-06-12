use crate::components::expandable::ExpandableText;
use serde_json::Value;
use yew::prelude::*;

pub(super) fn render_server_tool_use(name: &str, input: &Value) -> Html {
    let badge_label = if name.contains("web_search") || name.contains("search") {
        "Web Search"
    } else {
        "Server"
    };
    let args_summary = summarize_input(input);
    html! {
        <div class="tool-use server-tool-use">
            <div class="tool-use-header">
                <span class="tool-badge server">{ badge_label }</span>
                <span class="tool-name">{ name }</span>
                { if !args_summary.is_empty() {
                    html! { <span class="tool-meta">{ args_summary }</span> }
                } else {
                    html! {}
                }}
            </div>
        </div>
    }
}

pub(super) fn render_web_search_result(content: &Value) -> Html {
    let preview = serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
    html! {
        <div class="tool-result web-search-result">
            <div class="tool-use-header">
                <span class="tool-badge server">{ "Web Search Result" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={300} class="tool-result-content" />
        </div>
    }
}

pub(super) fn render_code_execution_result(content: &Value) -> Html {
    let preview = serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
    html! {
        <div class="tool-result code-execution-result">
            <div class="tool-use-header">
                <span class="tool-badge code-exec">{ "Code Execution" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={500} class="tool-result-content" />
        </div>
    }
}

pub(super) fn render_mcp_tool_use(name: &str, server_name: Option<&str>, input: &Value) -> Html {
    let display_name = match server_name {
        Some(server) => format!("{} > {}", server, name),
        None => name.to_string(),
    };
    let args_summary = summarize_input(input);
    html! {
        <div class="tool-use mcp-tool-use">
            <div class="tool-use-header">
                <span class="tool-badge mcp">{ "MCP" }</span>
                <span class="tool-name">{ display_name }</span>
                { if !args_summary.is_empty() {
                    html! { <span class="tool-meta">{ args_summary }</span> }
                } else {
                    html! {}
                }}
            </div>
        </div>
    }
}

pub(super) fn render_mcp_tool_result(content: &Value, is_error: bool) -> Html {
    let class = if is_error {
        "tool-result mcp-tool-result error"
    } else {
        "tool-result mcp-tool-result"
    };
    let preview = serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
    html! {
        <div class={class}>
            <div class="tool-use-header">
                <span class={if is_error { "tool-badge mcp error" } else { "tool-badge mcp" }}>
                    { if is_error { "MCP Error" } else { "MCP Result" } }
                </span>
            </div>
            <ExpandableText full_text={preview} max_len={500} class="tool-result-content" />
        </div>
    }
}

pub(super) fn render_container_upload(data: &Value) -> Html {
    let preview = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    html! {
        <div class="tool-use container-upload">
            <div class="tool-use-header">
                <span class="tool-badge container">{ "Container Upload" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={300} class="tool-result-content" />
        </div>
    }
}

pub(super) fn render_unknown_block(value: &Value) -> Html {
    let preview = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    html! {
        <div class="tool-use unknown-block">
            <div class="tool-use-header">
                <span class="tool-badge unknown">{ "Unknown Block" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={300} class="tool-result-content" />
        </div>
    }
}

fn summarize_input(input: &Value) -> String {
    if let Some(obj) = input.as_object() {
        let entries: Vec<String> = obj
            .iter()
            .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
            .take(3)
            .map(|(k, v)| match v {
                Value::String(s) => {
                    let truncated = if s.len() > 40 {
                        format!("{}...", shared::fmt::truncate_str(s, 40))
                    } else {
                        s.clone()
                    };
                    format!("{}={}", k, truncated)
                }
                other => format!("{}={}", k, other),
            })
            .collect();
        entries.join(", ")
    } else {
        String::new()
    }
}

pub(super) fn render_structured_block(block: &shared::ContentBlock) -> Html {
    match block {
        shared::ContentBlock::Image(_) => {
            html! { <span class="tool-result-image-tag">{ "[image]" }</span> }
        }
        shared::ContentBlock::Text(t) => {
            html! { <ExpandableText full_text={t.text.clone()} max_len={500} class="tool-result-content" /> }
        }
        other => {
            let json = serde_json::to_string_pretty(other).unwrap_or_default();
            html! { <pre class="tool-result-content">{ json }</pre> }
        }
    }
}
