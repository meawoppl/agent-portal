mod bash;
mod edit;
mod interactive;
mod search;
mod task;

use serde_json::Value;
use shared::{
    AskUserQuestionInput, BashInput, EditInput, ExitPlanModeInput, GlobInput, GrepInput, ReadInput,
    TaskInput, TodoWriteInput, ToolInput, WebFetchInput, WebSearchInput, WriteInput,
};
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

/// Project-local equivalent of `TryFrom<ToolInput>` for the upstream
/// `claude_codes::tool_inputs::*Input` variant structs. Orphan rules forbid an
/// `impl TryFrom<ToolInput> for BashInput` here (both sides are foreign), so
/// each variant struct implements this local trait instead. The blanket impl
/// in [`extract_tool_input`] keeps call sites to one line.
pub trait FromToolInput: Sized {
    fn from_tool_input(input: ToolInput) -> Option<Self>;
}

macro_rules! impl_from_tool_input {
    ($variant:ident, $inner:ty) => {
        impl FromToolInput for $inner {
            fn from_tool_input(input: ToolInput) -> Option<Self> {
                match input {
                    ToolInput::$variant(v) => Some(v),
                    _ => None,
                }
            }
        }
    };
}

impl_from_tool_input!(Bash, BashInput);
impl_from_tool_input!(Read, ReadInput);
impl_from_tool_input!(Edit, EditInput);
impl_from_tool_input!(Write, WriteInput);
impl_from_tool_input!(Glob, GlobInput);
impl_from_tool_input!(Grep, GrepInput);
impl_from_tool_input!(Task, TaskInput);
impl_from_tool_input!(WebFetch, WebFetchInput);
impl_from_tool_input!(WebSearch, WebSearchInput);
impl_from_tool_input!(TodoWrite, TodoWriteInput);
impl_from_tool_input!(AskUserQuestion, AskUserQuestionInput);
impl_from_tool_input!(ExitPlanMode, ExitPlanModeInput);

/// Decode a tool-use `input` JSON envelope into the typed variant struct `T`.
/// Returns `None` when the JSON does not deserialize as the expected
/// `ToolInput` variant — callers should fall back to a generic renderer.
pub fn extract_tool_input<T: FromToolInput>(input: &Value) -> Option<T> {
    serde_json::from_value::<ToolInput>(input.clone())
        .ok()
        .and_then(T::from_tool_input)
}

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
