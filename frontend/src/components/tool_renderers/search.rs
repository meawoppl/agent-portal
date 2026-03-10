use serde_json::Value;
use yew::prelude::*;

use crate::components::expandable::ExpandableText;

pub fn render_glob_tool(input: &Value) -> Html {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
    let path = input.get("path").and_then(|v| v.as_str());

    html! {
        <div class="tool-use glob-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "🔍" }</span>
                <span class="tool-name">{ "Glob" }</span>
                <code class="glob-pattern-inline">{ pattern }</code>
                {
                    if let Some(p) = path {
                        html! { <span class="tool-meta">{ format!("in {}", p) }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    }
}

pub fn render_grep_tool(input: &Value) -> Html {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
    let path = input.get("path").and_then(|v| v.as_str());
    let glob = input.get("glob").and_then(|v| v.as_str());
    let file_type = input.get("type").and_then(|v| v.as_str());
    let case_insensitive = input.get("-i").and_then(|v| v.as_bool()).unwrap_or(false);

    let has_options = glob.is_some() || file_type.is_some();

    html! {
        <div class="tool-use grep-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "🔎" }</span>
                <span class="tool-name">{ "Grep" }</span>
                <code class="grep-pattern-inline">{ format!("/{}/", pattern) }</code>
                {
                    if case_insensitive {
                        html! { <span class="tool-badge">{ "-i" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(p) = path {
                        html! { <span class="tool-meta">{ format!("in {}", p) }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            {
                if has_options {
                    html! {
                        <div class="grep-options">
                            {
                                if let Some(g) = glob {
                                    html! { <span class="grep-option">{ format!("--glob={}", g) }</span> }
                                } else {
                                    html! {}
                                }
                            }
                            {
                                if let Some(t) = file_type {
                                    html! { <span class="grep-option">{ format!("--type={}", t) }</span> }
                                } else {
                                    html! {}
                                }
                            }
                        </div>
                    }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

pub fn render_webfetch_tool(input: &Value) -> Html {
    let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("?");
    let prompt = input.get("prompt").and_then(|v| v.as_str());

    html! {
        <div class="tool-use webfetch-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "🌐" }</span>
                <span class="tool-name">{ "WebFetch" }</span>
            </div>
            <div class="webfetch-url">
                <a href={url.to_string()} target="_blank" rel="noopener noreferrer">{ url }</a>
            </div>
            {
                if let Some(p) = prompt {
                    html! { <ExpandableText full_text={p.to_string()} max_len={100} class="webfetch-prompt" tag="div" /> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

pub fn render_websearch_tool(input: &Value) -> Html {
    let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("?");

    html! {
        <div class="tool-use websearch-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "🔍" }</span>
                <span class="tool-name">{ "WebSearch" }</span>
            </div>
            <div class="websearch-query">{ format!("\"{}\"", query) }</div>
        </div>
    }
}
