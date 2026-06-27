mod bash;
mod edit;
mod interactive;
mod search;
mod task;

use serde::de::DeserializeOwned;
use serde_json::Value;
use shared::{ReadInput, ToolInput};
use yew::prelude::*;

use self::bash::render_bash_tool;
use self::edit::{render_edit_tool, render_write_tool};
pub(crate) use self::interactive::{has_askuserquestion_answers, render_askuserquestion_result};
use self::interactive::{
    render_askuserquestion_tool, render_exitplanmode_tool, render_todowrite_tool,
};
use self::search::{
    render_glob_tool, render_grep_tool, render_toolsearch_tool, render_webfetch_tool,
    render_websearch_tool,
};
use self::task::render_task_tool;
use super::expandable::ExpandableText;

/// Decode a tool-use `input` JSON object directly into the typed struct `T`.
///
/// Callers that have the surrounding tool name should prefer
/// [`ToolInput::from_named_input`], which lets the SDK resolve ambiguous tool
/// shapes such as `WebSearch` vs. `ToolSearch`. This helper remains for cards
/// that already dispatch by a concrete tool name and only need a specific input
/// struct.
pub fn extract_tool_input<T: DeserializeOwned>(input: &Value) -> Option<T> {
    serde_json::from_value::<T>(input.clone()).ok()
}

/// Render a tool use block with special handling for various tools
pub fn render_tool_use(name: &str, input: &Value) -> Html {
    let named_input = ToolInput::from_named_input(name, input.clone());
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
        "WebSearch" => match &named_input {
            ToolInput::WebSearch(web_search) => render_websearch_tool(Some(web_search)),
            _ => render_websearch_tool(None),
        },
        "ToolSearch" => match &named_input {
            ToolInput::ToolSearch(tool_search) => {
                render_toolsearch_tool(&tool_search.query, tool_search.max_results)
            }
            _ => render_generic_tool(name, input),
        },
        _ => render_generic_tool(name, input),
    }
}

/// Render Read tool with file path and range info
fn render_read_tool(input: &Value) -> Html {
    let read = extract_tool_input::<ReadInput>(input).unwrap_or(ReadInput {
        file_path: "?".to_string(),
        offset: None,
        limit: None,
    });

    let range_info = match (read.offset, read.limit) {
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
                <span class="read-file-path">{ read.file_path }</span>
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

#[cfg(test)]
mod tests {
    use super::{extract_tool_input, render_tool_use};
    use shared::{GlobInput, GrepInput, ToolInput, WebSearchInput};

    /// Regression: a bare `{"query": …}` WebSearch input used to misclassify as
    /// `ToolSearch` in the untagged `ToolInput` enum (declared first, sharing the
    /// `query` field) and lose the query. The SDK's name-aware parser must
    /// recover it.
    #[test]
    fn websearch_bare_query_uses_sdk_name_aware_parser() {
        let input = serde_json::json!({ "query": "rust async traits" });
        let parsed = ToolInput::from_named_input("WebSearch", input);
        assert!(matches!(parsed, ToolInput::WebSearch(ref ws) if ws.query == "rust async traits"));
    }

    #[test]
    fn toolsearch_bare_query_uses_sdk_name_aware_parser() {
        let input = serde_json::json!({ "query": "select:Read,Edit" });
        let parsed = ToolInput::from_named_input("ToolSearch", input);
        assert!(matches!(parsed, ToolInput::ToolSearch(ref ts) if ts.query == "select:Read,Edit"));
    }

    /// A WebSearch with domain filters parsed fine even before the fix (the
    /// extra fields tripped `ToolSearch`'s `deny_unknown_fields`); pin it so the
    /// by-name path keeps working too.
    #[test]
    fn websearch_with_domains_extracts() {
        let input = serde_json::json!({ "query": "tokio", "allowed_domains": ["docs.rs"] });
        let ws: WebSearchInput = extract_tool_input(&input).expect("WebSearch should parse");
        assert_eq!(ws.query, "tokio");
        assert_eq!(
            ws.allowed_domains.as_deref(),
            Some(&["docs.rs".to_string()][..])
        );
    }

    #[test]
    fn websearch_renderer_accepts_bare_query() {
        let input = serde_json::json!({ "query": "rust async traits" });
        let _ = render_tool_use("WebSearch", &input);
    }

    /// Glob and Grep share the `pattern` field; by-name dispatch must still land
    /// each in its own struct.
    #[test]
    fn glob_and_grep_still_extract_by_name() {
        let glob: GlobInput = extract_tool_input(&serde_json::json!({ "pattern": "**/*.rs" }))
            .expect("Glob should parse");
        assert_eq!(glob.pattern, "**/*.rs");
        let grep: GrepInput = extract_tool_input(&serde_json::json!({ "pattern": "TODO" }))
            .expect("Grep should parse");
        assert_eq!(grep.pattern, "TODO");
    }
}
