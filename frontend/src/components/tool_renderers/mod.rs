mod bash;
mod edit;
mod interactive;
mod search;
mod task;

use serde_json::Value;
use yew::prelude::*;

use self::bash::render_bash_tool;
use self::edit::{render_edit_tool, render_write_tool};
use self::interactive::{
    render_askuserquestion_tool, render_exitplanmode_tool, render_todowrite_tool,
};
use self::search::{
    render_glob_tool, render_grep_tool, render_webfetch_tool, render_websearch_tool,
};
use self::task::render_task_tool;
use super::expandable::ExpandableText;

/// Render a tool use block with special handling for various tools
pub fn render_tool_use(name: &str, input: &Value) -> Html {
    match name {
        "Edit" => render_edit_tool(input),
        "Write" => render_write_tool(input),
        "TodoWrite" => render_todowrite_tool(input),
        "AskUserQuestion" => render_askuserquestion_tool(input),
        "ExitPlanMode" => render_exitplanmode_tool(input),
        "Bash" => render_bash_tool(input),
        "Read" => render_read_tool(input),
        "Glob" => render_glob_tool(input),
        "Grep" => render_grep_tool(input),
        "Task" => render_task_tool(input),
        "WebFetch" => render_webfetch_tool(input),
        "WebSearch" => render_websearch_tool(input),
        _ => render_generic_tool(name, input),
    }
}

/// Render Read tool with file path and range info
fn render_read_tool(input: &Value) -> Html {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let offset = input.get("offset").and_then(|v| v.as_i64());
    let limit = input.get("limit").and_then(|v| v.as_i64());

    let range_info = match (offset, limit) {
        (Some(o), Some(l)) => Some(format!("lines {}-{}", o, o + l)),
        (Some(o), None) => Some(format!("from line {}", o)),
        (None, Some(l)) => Some(format!("first {} lines", l)),
        _ => None,
    };

    html! {
        <div class="tool-use read-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "📖" }</span>
                <span class="tool-name">{ "Read" }</span>
                <span class="read-file-path">{ file_path }</span>
                {
                    if let Some(range) = range_info {
                        html! { <span class="tool-meta">{ range }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    }
}

/// Generic tool renderer for unrecognized tools
fn render_generic_tool(name: &str, input: &Value) -> Html {
    let args_html = render_generic_args(input);
    html! {
        <div class="tool-use">
            <div class="tool-use-header">
                <span class="tool-icon">{ "⚡" }</span>
                <span class="tool-name">{ name }</span>
            </div>
            <div class="tool-args">{ args_html }</div>
        </div>
    }
}

/// Render generic tool arguments as expandable Html.
fn render_generic_args(input: &Value) -> Html {
    if let Some(obj) = input.as_object() {
        let entries: Vec<(&String, &Value)> = obj
            .iter()
            .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
            .take(3)
            .collect();
        if entries.is_empty() {
            return html! { "..." };
        }
        html! {
            { for entries.into_iter().map(|(k, v)| {
                match v {
                    Value::String(s) => html! {
                        <span class="tool-arg-entry">
                            { format!("{}=", k) }
                            <ExpandableText full_text={s.clone()} max_len={40} tag="span" />
                        </span>
                    },
                    other => html! {
                        <span class="tool-arg-entry">{ format!("{}={}", k, other) }</span>
                    },
                }
            })}
        }
    } else {
        html! { "..." }
    }
}
