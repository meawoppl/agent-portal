//! Ask an agent for a handoff summary while it still has usage left.
//!
//! When an agent reports it is close to its usage limit (Claude's
//! `allowed_warning` rate-limit event, a Codex rate-limit snapshot past
//! [`shared::usage_limit::CODEX_USAGE_WARNING_PERCENT`]), Portal queues one
//! request per usage window in the session's work queue. The queue delivers
//! it after the current turn ends, so the agent's work is not interrupted,
//! and the user sees it there and can delete it. A request can't reach an
//! agent that has already run out: the queue holds while a session is
//! throttled.
//!
//! The agent answers under [`shared::usage_limit::HANDOFF_SUMMARY_HEADING`]
//! and the reply stays in the transcript, where a later handoff
//! ([`super::session_handoff`]) finds it. That summary carries what the raw
//! transcript can't: decisions the agent made and never wrote down.

use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use shared::usage_limit::HANDOFF_SUMMARY_HEADING;
use shared::AgentType;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::NewSessionEditStackItem;
use crate::schema::{messages, session_edit_stack_items, sessions};

const TITLE: &str = "Handoff summary";
const KIND: &str = "handoff_summary";
/// Longest summary carried into a handoff note.
const MAX_SUMMARY_CHARS: usize = 6_000;
/// Rows matching the heading that are inspected; the newest real reply wins.
const SUMMARY_CANDIDATES: i64 = 5;

/// Stored as the queue item's `source`: marks it as Portal's own request and
/// names the usage window it was made for.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryRequestSource {
    portal_request: String,
    usage_window: String,
}

fn request_window(source: Option<&serde_json::Value>) -> Option<String> {
    let source = serde_json::from_value::<SummaryRequestSource>(source?.clone()).ok()?;
    (source.portal_request == KIND).then_some(source.usage_window)
}

/// Whether a queue item is a summary request. Such items are sent as written,
/// without the edit-stack annotation wrapper.
pub(crate) fn is_summary_request(source: Option<&serde_json::Value>) -> bool {
    request_window(source).is_some()
}

fn summary_prompt() -> String {
    format!(
        "Your usage is close to its limit, so another agent may soon have to take over this work. \
         While you still can, reply with a handoff summary for that agent. Start with the heading \
         `{HANDOFF_SUMMARY_HEADING}` and cover, briefly:\n\
         - the goal, in the user's terms;\n\
         - decisions made and why, including approaches you rejected;\n\
         - current state: what is done, the branch, and any uncommitted changes;\n\
         - what remains, in order;\n\
         - anything surprising the next agent should know.\n\n\
         Keep it under 400 words. Do not do any other work in this turn."
    )
}

/// Queue a summary request for `window` unless one was already made for it
/// (in any state: sent, pending, or deleted by the user).
pub(crate) fn request_handoff_summary(
    db_pool: &crate::db::DbPool,
    session_manager: &crate::handlers::websocket::SessionManager,
    session_key: &str,
    session_id: Uuid,
    window: String,
) -> Result<(), AppError> {
    let mut conn = db_pool.get()?;
    let owner = sessions::table
        .find(session_id)
        .select(sessions::user_id)
        .first::<Uuid>(&mut conn)?;
    let sources: Vec<Option<serde_json::Value>> = session_edit_stack_items::table
        .filter(session_edit_stack_items::session_id.eq(session_id))
        .filter(session_edit_stack_items::title.eq(TITLE))
        .select(session_edit_stack_items::source)
        .load(&mut conn)?;
    if sources
        .iter()
        .any(|source| request_window(source.as_ref()).as_deref() == Some(window.as_str()))
    {
        return Ok(());
    }
    let source = SummaryRequestSource {
        portal_request: KIND.into(),
        usage_window: window,
    };
    diesel::insert_into(session_edit_stack_items::table)
        .values(&NewSessionEditStackItem {
            session_id,
            created_by: owner,
            title: TITLE.into(),
            body: summary_prompt(),
            source: Some(
                serde_json::to_value(source)
                    .map_err(|e| AppError::Internal(format!("handoff summary source: {e}")))?,
            ),
            context: None,
            image_data_url: None,
        })
        .execute(&mut conn)?;
    drop(conn);
    super::edit_stack::broadcast_edit_stack_items(
        db_pool,
        session_manager,
        session_key,
        session_id,
    )?;
    // Sends now only if the session is already idle; otherwise the turn's
    // end releases it.
    super::edit_stack::try_send_next_pending_edit_stack_item(
        db_pool,
        session_manager,
        session_key,
        session_id,
    )?;
    Ok(())
}

/// Drop unsent summary requests: once the work has been handed off, a request
/// left pending would fire when the source is resumed, long after it matters.
pub(crate) fn cancel_pending_requests(
    conn: &mut PgConnection,
    session_id: Uuid,
) -> QueryResult<usize> {
    let pending: Vec<(Uuid, Option<serde_json::Value>)> = session_edit_stack_items::table
        .filter(session_edit_stack_items::session_id.eq(session_id))
        .filter(session_edit_stack_items::status.eq("pending"))
        .filter(session_edit_stack_items::title.eq(TITLE))
        .select((
            session_edit_stack_items::id,
            session_edit_stack_items::source,
        ))
        .load(conn)?;
    let ids: Vec<Uuid> = pending
        .into_iter()
        .filter(|(_, source)| is_summary_request(source.as_ref()))
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::update(session_edit_stack_items::table.filter(session_edit_stack_items::id.eq_any(ids)))
        .set((
            session_edit_stack_items::status.eq("deleted"),
            session_edit_stack_items::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)
}

/// The newest handoff summary the session's agent wrote, from its heading on.
pub(crate) fn latest_summary(
    conn: &mut PgConnection,
    session_id: Uuid,
) -> QueryResult<Option<String>> {
    let rows: Vec<(String, String)> = messages::table
        .filter(messages::session_id.eq(session_id))
        // The request itself quotes the heading; only agent output counts.
        .filter(messages::role.ne("user"))
        .filter(messages::content.like(format!("%{HANDOFF_SUMMARY_HEADING}%")))
        .order((messages::created_at.desc(), messages::id.desc()))
        .limit(SUMMARY_CANDIDATES)
        .select((messages::agent_type, messages::content))
        .load(conn)?;
    Ok(rows.iter().find_map(|(agent, content)| {
        summary_from_reply(AgentType::parse_or_default(agent), content)
    }))
}

fn summary_from_reply(agent: AgentType, content: &str) -> Option<String> {
    let text = agent_reply_text(agent, content)?;
    let start = text.find(HANDOFF_SUMMARY_HEADING)?;
    let summary = text[start..].trim();
    Some(shared::strings::truncate_with_ellipsis(
        summary,
        MAX_SUMMARY_CHARS,
    ))
}

/// Full text of an agent reply row (not the one-line peek excerpt).
fn agent_reply_text(agent: AgentType, content: &str) -> Option<String> {
    match agent {
        AgentType::Claude => {
            use claude_codes::io::ContentBlock;
            let claude_codes::ClaudeOutput::Assistant(message) =
                serde_json::from_str::<claude_codes::ClaudeOutput>(content).ok()?
            else {
                return None;
            };
            let text: Vec<&str> = message
                .message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        AgentType::Codex => codex_reply_text(content),
        AgentType::Muse | AgentType::Antigravity => None,
    }
}

/// Codex replies are stored in either the exec item shape or the app-server
/// one (`agentMessage`); accept both.
fn codex_reply_text(content: &str) -> Option<String> {
    use codex_codes::io::items::ThreadItem;
    if let Ok(codex_codes::ThreadEvent::ItemCompleted(event)) =
        serde_json::from_str::<codex_codes::ThreadEvent>(content)
    {
        if let ThreadItem::AgentMessage(message) = event.item {
            return Some(message.text);
        }
    }
    #[derive(Deserialize)]
    struct Completed {
        #[serde(rename = "type")]
        kind: String,
        item: codex_codes::protocol::ThreadItem,
    }
    let completed = serde_json::from_str::<Completed>(content).ok()?;
    match completed.item.payload {
        codex_codes::protocol::ThreadItemPayload::AgentMessage { text, .. }
            if completed.kind == "item.completed" =>
        {
            Some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_is_cut_from_its_heading_in_either_codex_shape() {
        for content in [
            r###"{"type":"item.completed","item":{"id":"m","type":"agent_message","text":"Sure.\n\n## Handoff summary\nGoal: ship it."}}"###,
            r###"{"type":"item.completed","item":{"id":"m","type":"agentMessage","phase":"final_answer","text":"Sure.\n\n## Handoff summary\nGoal: ship it."}}"###,
        ] {
            assert_eq!(
                summary_from_reply(AgentType::Codex, content).as_deref(),
                Some("## Handoff summary\nGoal: ship it."),
                "{content}"
            );
        }
    }

    #[test]
    fn claude_summary_joins_text_blocks() {
        let content = r###"{"type":"assistant","session_id":"s","message":{"id":"m","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"## Handoff summary"},{"type":"text","text":"Next: tests."}]}}"###;
        assert_eq!(
            summary_from_reply(AgentType::Claude, content).as_deref(),
            Some("## Handoff summary\nNext: tests.")
        );
        let unrelated = content.replace("## Handoff summary", "Working on it");
        assert_eq!(summary_from_reply(AgentType::Claude, &unrelated), None);
    }

    #[test]
    fn only_portal_requests_are_recognized_and_keyed_by_window() {
        let source = serde_json::to_value(SummaryRequestSource {
            portal_request: KIND.into(),
            usage_window: "claude:five_hour:1".into(),
        })
        .unwrap();
        assert_eq!(
            request_window(Some(&source)).as_deref(),
            Some("claude:five_hour:1")
        );
        let annotation: serde_json::Value =
            serde_json::from_str(r#"{"kind":"forward","url":"http://x"}"#).unwrap();
        assert!(!is_summary_request(Some(&annotation)));
        assert!(!is_summary_request(None));
        assert!(summary_prompt().contains(HANDOFF_SUMMARY_HEADING));
    }
}
