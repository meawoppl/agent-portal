//! Admin raw messages tab — raw message viewer

use crate::utils;
use uuid::Uuid;
use web_sys::MouseEvent;
use yew::prelude::*;

use super::RawMessageLogInfo;

#[derive(Properties, PartialEq)]
struct RawMessageRowProps {
    message: RawMessageLogInfo,
    on_delete: Callback<Uuid>,
    on_view: Callback<RawMessageLogInfo>,
}

#[function_component(RawMessageRow)]
fn raw_message_row(props: &RawMessageRowProps) -> Html {
    let msg = &props.message;

    let on_delete = {
        let callback = props.on_delete.clone();
        let msg_id = msg.id;
        Callback::from(move |_: MouseEvent| callback.emit(msg_id))
    };

    let on_view = {
        let callback = props.on_view.clone();
        let message = msg.clone();
        Callback::from(move |_: MouseEvent| callback.emit(message.clone()))
    };

    // Get message type from content if available
    let msg_type = msg
        .message_content
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");

    // Truncate session ID for display
    let session_id_display = msg
        .session_id
        .map(|id| format!("{}...", &id.to_string()[..8]))
        .unwrap_or_else(|| "-".to_string());

    html! {
        <tr>
            <td class="timestamp">{ utils::format_timestamp(&msg.created_at) }</td>
            <td class="raw-msg-type">{ msg_type }</td>
            <td class="raw-msg-source">{ &msg.message_source }</td>
            <td class="raw-msg-reason">{ msg.render_reason.as_deref().unwrap_or("-") }</td>
            <td class="raw-msg-session" title={msg.session_id.map(|id| id.to_string()).unwrap_or_default()}>
                { session_id_display }
            </td>
            <td class="actions">
                <button class="view-btn" onclick={on_view} title="View message content">
                    { "View" }
                </button>
                <button class="delete-btn" onclick={on_delete} title="Delete">
                    { "Delete" }
                </button>
            </td>
        </tr>
    }
}

#[derive(Properties, PartialEq)]
pub struct AdminRawMessagesTabProps {
    pub raw_messages: Vec<RawMessageLogInfo>,
    pub on_delete: Callback<Uuid>,
    pub on_view: Callback<RawMessageLogInfo>,
}

#[function_component(AdminRawMessagesTab)]
pub fn admin_raw_messages_tab(props: &AdminRawMessagesTabProps) -> Html {
    html! {
        <div class="admin-raw-messages">
            <p class="raw-messages-description">
                { "Messages that failed to parse and were rendered as raw JSON are logged here for debugging." }
            </p>
            {
                if props.raw_messages.is_empty() {
                    html! {
                        <p class="no-raw-messages">{ "No raw messages logged yet." }</p>
                    }
                } else {
                    html! {
                        <table class="admin-table">
                            <thead>
                                <tr>
                                    <th>{ "Time" }</th>
                                    <th>{ "Type" }</th>
                                    <th>{ "Source" }</th>
                                    <th>{ "Reason" }</th>
                                    <th>{ "Session" }</th>
                                    <th>{ "Actions" }</th>
                                </tr>
                            </thead>
                            <tbody>
                                {
                                    props.raw_messages.iter().map(|msg| {
                                        html! {
                                            <RawMessageRow
                                                key={msg.id.to_string()}
                                                message={msg.clone()}
                                                on_delete={props.on_delete.clone()}
                                                on_view={props.on_view.clone()}
                                            />
                                        }
                                    }).collect::<Html>()
                                }
                            </tbody>
                        </table>
                    }
                }
            }
        </div>
    }
}
