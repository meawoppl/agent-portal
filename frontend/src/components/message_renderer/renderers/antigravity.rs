use serde_json::Value;
use yew::prelude::*;

pub fn render_antigravity_frame(value: &Value) -> Html {
    let Some(body) = render_antigravity_frame_content(value) else {
        return html! {};
    };
    let badge = if value.get("type").and_then(Value::as_str) == Some("antigravity_turn_completed") {
        "Done"
    } else {
        value
            .get("step_kind")
            .and_then(Value::as_str)
            .unwrap_or("Event")
    };
    html! {
        <div class="claude-message antigravity-message">
            <div class="message-header">
                <span class="message-type-badge antigravity">{ "Antigravity" }</span>
                <span class="antigravity-kind">{ badge }</span>
                { status_chip(value) }
            </div>
            <div class="message-body">{ body }</div>
        </div>
    }
}

pub fn render_antigravity_frame_content(value: &Value) -> Option<Html> {
    match value.get("type").and_then(Value::as_str) {
        Some("antigravity_turn_completed") => {
            let model = text(value, "model").unwrap_or("model unknown");
            let stop = text(value, "stop_reason").unwrap_or("complete");
            let is_error = value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(html! {
                <div class={classes!("antigravity-terminal", is_error.then_some("error"))}>
                    <span>{ model }</span>
                    <span>{ stop }</span>
                </div>
            })
        }
        Some("antigravity_event") => match value.get("event").and_then(Value::as_str) {
            Some("step_update") => Some(render_step_update(value)),
            Some("trajectory_state_update") => Some(render_trajectory_update(value)),
            _ => Some(render_unknown_event(value)),
        },
        _ => None,
    }
}

fn render_step_update(value: &Value) -> Html {
    let frame_text = text(value, "text")
        .filter(|s| !s.is_empty())
        .or_else(|| text(value, "text_delta").filter(|s| !s.is_empty()));
    let thinking = text(value, "thinking").filter(|s| !s.is_empty());
    let step_kind = text(value, "step_kind").unwrap_or("Message");
    let target = text(value, "target").unwrap_or("");
    let error = text(value, "error_message").filter(|s| !s.is_empty());
    let action = action_summary(value);

    html! {
        <>
            <div class="antigravity-meta">
                <span>{ step_kind }</span>
                if !target.is_empty() {
                    <span>{ target.replace("STEP_UPDATE_TARGET_", "").to_lowercase() }</span>
                }
                if value.get("is_main").and_then(Value::as_bool).unwrap_or(false) {
                    <span>{ "main" }</span>
                }
            </div>
            if let Some(text) = frame_text {
                <div class="antigravity-text">{ crate::components::markdown::render_markdown(text) }</div>
            }
            if let Some(thinking) = thinking {
                <details class="antigravity-thinking">
                    <summary>{ "thinking" }</summary>
                    <pre>{ thinking }</pre>
                </details>
            }
            if let Some(action) = action {
                <pre class="antigravity-action">{ action }</pre>
            }
            if let Some(error) = error {
                <div class="antigravity-error">{ error }</div>
            }
        </>
    }
}

fn render_trajectory_update(value: &Value) -> Html {
    let state = text(value, "state").unwrap_or("STATE_UNSPECIFIED");
    let err = text(value, "error").filter(|s| !s.is_empty());
    html! {
        <div class="antigravity-trajectory">
            <span>{ state }</span>
            if let Some(err) = err {
                <span class="antigravity-error">{ err }</span>
            }
        </div>
    }
}

fn render_unknown_event(value: &Value) -> Html {
    let display = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    html! { <pre class="antigravity-action">{ display }</pre> }
}

fn status_chip(value: &Value) -> Html {
    let Some(state) = text(value, "state") else {
        return html! {};
    };
    let class = if state.contains("DONE") || state.contains("FULLY_IDLE") {
        "done"
    } else if state.contains("ERROR") || state.contains("CANCELLED") {
        "error"
    } else {
        "active"
    };
    html! { <span class={classes!("antigravity-status", class)}>{ compact_state(state) }</span> }
}

fn action_summary(value: &Value) -> Option<String> {
    let update = value.get("update")?;
    if let Some(cmd) = update
        .get("runCommand")
        .or_else(|| update.get("run_command"))
        .and_then(|v| v.get("commandLine").or_else(|| v.get("command_line")))
        .and_then(Value::as_str)
    {
        return Some(format!("$ {cmd}"));
    }
    for (field, label, key) in [
        ("viewFile", "view", "filePath"),
        ("view_file", "view", "file_path"),
        ("editFile", "edit", "filePath"),
        ("edit_file", "edit", "file_path"),
        ("createFile", "create", "filePath"),
        ("create_file", "create", "file_path"),
        ("listDirectory", "list", "directory"),
        ("list_directory", "list", "directory"),
        ("searchDirectory", "search", "query"),
        ("search_directory", "search", "query"),
        ("findFile", "find", "pattern"),
        ("find_file", "find", "pattern"),
    ] {
        if let Some(arg) = update
            .get(field)
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(format!("{label}: {arg}"));
        }
    }
    None
}

fn compact_state(state: &str) -> String {
    state
        .trim_start_matches("STATE_")
        .replace('_', " ")
        .to_ascii_lowercase()
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
