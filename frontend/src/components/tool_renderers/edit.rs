use serde_json::Value;
use shared::{EditInput, MultiEditInput, WriteInput};
use yew::prelude::*;

use crate::components::diff::{DiffCard, DiffSource};
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

    let source = DiffSource::OldNew {
        old: edit.old_string,
        new: edit.new_string,
    };

    html! {
        <DiffCard
            {source}
            file_path={AttrValue::from(edit.file_path)}
            {replace_all}
        />
    }
}

pub fn render_multiedit_tool(input: &Value) -> Html {
    let multi = extract_tool_input::<MultiEditInput>(input).unwrap_or(MultiEditInput {
        file_path: "unknown file".to_string(),
        edits: Vec::new(),
    });
    let file_path = AttrValue::from(multi.file_path);
    let edit_count = multi.edits.len();

    html! {
        <div class="tool-use multiedit-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "\u{270f}\u{fe0f}" }</span>
                <span class="tool-name">{ "MultiEdit" }</span>
                <span class="edit-file-path">{ &file_path }</span>
                <span class="tool-meta">
                    { format!("({} edit{})", edit_count, if edit_count == 1 { "" } else { "s" }) }
                </span>
            </div>
            {
                multi.edits.into_iter().map(|edit| {
                    let source = DiffSource::OldNew {
                        old: edit.old_string,
                        new: edit.new_string,
                    };
                    html! { <DiffCard {source} /> }
                }).collect::<Html>()
            }
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
