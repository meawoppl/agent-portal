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
        .filter(|s| shared::strings::is_non_empty(s))
        .or_else(|| text(value, "text_delta").filter(|s| shared::strings::is_non_empty(s)));
    let thinking = text(value, "thinking").filter(|s| shared::strings::is_non_empty(s));
    let step_kind = text(value, "step_kind").unwrap_or("Message");
    let target = text(value, "target").unwrap_or("");
    let error = text(value, "error_message").filter(|s| shared::strings::is_non_empty(s));
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

#[derive(serde::Deserialize, Default)]
struct TrajectoryDetails {
    state: Option<String>,
    error: Option<String>,
    error_code: Option<String>,
}

fn render_trajectory_update(value: &Value) -> Html {
    let details: TrajectoryDetails = serde_json::from_value(value.clone()).unwrap_or_default();
    html! {
        <div class="antigravity-trajectory">
            <span>{ details.state.as_deref().unwrap_or("STATE_UNSPECIFIED") }</span>
            if let Some(code) = details.error_code.filter(|s| !s.is_empty()) {
                <span class="antigravity-error">{ code }</span>
            }
            if let Some(err) = details.error.filter(|s| !s.is_empty()) {
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
    #[derive(serde::Deserialize)]
    struct StepDetails {
        update: antigravity_codes::protocol::StepUpdate,
    }
    let details: StepDetails = serde_json::from_value(value.clone()).ok()?;
    let update = details.update;
    if let Some(command) = update.run_command.and_then(|action| action.command_line) {
        return Some(format!("$ {command}"));
    }
    if let Some(action) = update.read_url_content {
        return match (
            action.title.filter(|s| !s.is_empty()),
            action.url.filter(|s| !s.is_empty()),
        ) {
            (Some(title), Some(url)) => Some(format!("fetch: {title} ({url})")),
            (Some(title), None) => Some(format!("fetch: {title}")),
            (None, Some(url)) => Some(format!("fetch: {url}")),
            (None, None) => Some("fetch URL content".into()),
        };
    }
    for (label, value) in [
        ("view", update.view_file.and_then(|a| a.file_path)),
        ("edit", update.edit_file.and_then(|a| a.file_path)),
        ("create", update.create_file.and_then(|a| a.file_path)),
        ("list", update.list_directory.and_then(|a| a.directory_path)),
        ("search", update.search_directory.and_then(|a| a.query)),
        ("find", update.find_file.and_then(|a| a.query)),
    ] {
        if let Some(value) = value.filter(|s| !s.is_empty()) {
            return Some(format!("{label}: {value}"));
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

#[cfg(test)]
mod typed_activity_tests {
    use super::*;
    #[test]
    fn url_fetch_summary_accepts_typed_camel_and_snake_case() {
        for wire in [
            r#"{"update":{"readUrlContent":{"url":"https://example.com","title":"Reference"}}}"#,
            r#"{"update":{"read_url_content":{"url":"https://example.com","title":"Reference"}}}"#,
        ] {
            assert_eq!(
                action_summary(&serde_json::from_str(wire).unwrap()).as_deref(),
                Some("fetch: Reference (https://example.com)")
            );
        }
    }
    #[test]
    fn trajectory_keeps_machine_error_code_without_human_message() {
        let details: TrajectoryDetails =
            serde_json::from_str(r#"{"state":"CANCELLED","error_code":"CAPACITY_UNAVAILABLE"}"#)
                .unwrap();
        assert_eq!(details.error_code.as_deref(), Some("CAPACITY_UNAVAILABLE"));
        assert!(details.error.is_none());
    }
}
