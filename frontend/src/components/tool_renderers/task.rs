use serde_json::Value;
use shared::{TaskInput, ToolInput};
use yew::prelude::*;

pub fn render_task_tool(input: &Value) -> Html {
    let task: Option<TaskInput> = serde_json::from_value::<ToolInput>(input.clone())
        .ok()
        .and_then(|t| match t {
            ToolInput::Task(task) => Some(task),
            _ => None,
        });

    let description: &str = task.as_ref().map(|t| t.description.as_str()).unwrap_or("?");
    let agent_type: &str = task
        .as_ref()
        .map(|t| t.subagent_type.as_str())
        .unwrap_or("agent");
    let background: bool = task
        .as_ref()
        .and_then(|t| t.run_in_background)
        .unwrap_or(false);

    html! {
        <div class="tool-use task-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "🤖" }</span>
                <span class="tool-name">{ "Task" }</span>
                <span class="task-agent-type">{ agent_type }</span>
                {
                    if background {
                        html! { <span class="tool-badge background">{ "background" }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="task-description">{ description }</div>
        </div>
    }
}
