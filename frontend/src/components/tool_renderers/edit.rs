use serde_json::Value;
use shared::{EditInput, WriteInput};
use yew::prelude::*;

use crate::components::diff::render_diff_lines;
use crate::components::expandable::ExpandableLines;
use crate::components::tool_renderers::extract_tool_input;

pub fn render_edit_tool(input: &Value) -> Html {
    let edit = extract_tool_input::<EditInput>(input).unwrap_or(EditInput {
        file_path: "unknown file".to_string(),
        old_string: String::new(),
        new_string: String::new(),
        replace_all: None,
    });
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
    let write = extract_tool_input::<WriteInput>(input).unwrap_or(WriteInput {
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
