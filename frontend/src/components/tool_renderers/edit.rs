use serde_json::Value;
use yew::prelude::*;

use crate::components::diff::render_diff_lines;
use crate::components::expandable::ExpandableLines;

pub fn render_edit_tool(input: &Value) -> Html {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown file");
    let old_string = input
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_string = input
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let replace_all = input
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let diff_html = render_diff_lines(old_string, new_string);

    html! {
        <div class="tool-use edit-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "✏️" }</span>
                <span class="tool-name">{ "Edit" }</span>
                <span class="edit-file-path">{ file_path }</span>
                {
                    if replace_all {
                        html! { <span class="edit-replace-all">{ "(replace all)" }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="diff-container">
                { diff_html }
            </div>
        </div>
    }
}

pub fn render_write_tool(input: &Value) -> Html {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown file");
    let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");

    let total_lines = content.lines().count();

    html! {
        <div class="tool-use write-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "📝" }</span>
                <span class="tool-name">{ "Write" }</span>
                <span class="write-file-path">{ file_path }</span>
                <span class="write-size">{ format!("({} lines, {} bytes)", total_lines, content.len()) }</span>
            </div>
            <div class="write-preview">
                <ExpandableLines content={content.to_string()} max_lines={20} />
            </div>
        </div>
    }
}
