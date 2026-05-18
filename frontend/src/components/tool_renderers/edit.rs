use serde_json::Value;
use shared::{EditInput, ToolInput, WriteInput};
use yew::prelude::*;

use crate::components::diff::render_diff_lines;
use crate::components::expandable::ExpandableLines;

pub fn render_edit_tool(input: &Value) -> Html {
    let edit = match serde_json::from_value::<ToolInput>(input.clone()) {
        Ok(ToolInput::Edit(e)) => Some(e),
        _ => None,
    };
    let fallback = EditInput {
        file_path: "unknown file".to_string(),
        old_string: String::new(),
        new_string: String::new(),
        replace_all: None,
    };
    let edit = edit.unwrap_or(fallback);
    let replace_all = edit.replace_all.unwrap_or(false);

    let diff_html = render_diff_lines(&edit.old_string, &edit.new_string);

    html! {
        <div class="tool-use edit-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "✏️" }</span>
                <span class="tool-name">{ "Edit" }</span>
                <span class="edit-file-path">{ &edit.file_path }</span>
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
    let write = match serde_json::from_value::<ToolInput>(input.clone()) {
        Ok(ToolInput::Write(w)) => Some(w),
        _ => None,
    };
    let write = write.unwrap_or(WriteInput {
        file_path: "unknown file".to_string(),
        content: String::new(),
    });

    let total_lines = write.content.lines().count();

    html! {
        <div class="tool-use write-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "📝" }</span>
                <span class="tool-name">{ "Write" }</span>
                <span class="write-file-path">{ &write.file_path }</span>
                <span class="write-size">{ format!("({} lines, {} bytes)", total_lines, write.content.len()) }</span>
            </div>
            <div class="write-preview">
                <ExpandableLines content={write.content.clone()} max_lines={20} />
            </div>
        </div>
    }
}
