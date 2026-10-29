//! Read-only discovery through Codex's supported protocol, never its private
//! SQLite schema or rollout layout. Importing does not resume or run a thread.
use crate::events::{ItemEvent, TurnCompletedEvent, UserEchoEvent};
use anyhow::{bail, Context, Result};
use codex_codes::{
    AppServerBuilder, AsyncClient, ClientInfo, InitializeCapabilities, InitializeParams,
    SortDirection, Thread, ThreadHistoryMode, ThreadListCwdFilter, ThreadListParams,
    ThreadListResponse, ThreadReadParams, ThreadReadResponse, ThreadSortKey, ThreadSourceKind,
    ThreadStatus, ThreadTurnsListParams, Turn, TurnItemsView, UserInput,
};
use shared::api::{
    ExternalSession, ExternalSessionList, ExternalSessionQuery, ImportedMessage,
    PreparedSessionImport, MAX_IMPORT_HISTORY_BYTES, MAX_IMPORT_MESSAGES,
};
use std::collections::HashSet;

async fn client() -> Result<AsyncClient> {
    let mut client = AsyncClient::spawn(AppServerBuilder::new()).await?;
    client
        .initialize(&InitializeParams {
            client_info: ClientInfo {
                name: "agent-portal-import".into(),
                title: Some("Portal session import".into()),
                version: shared::VERSION.into(),
            },
            capabilities: Some(InitializeCapabilities {
                experimental_api: Some(true),
                ..Default::default()
            }),
        })
        .await?;
    Ok(client)
}

fn summary(thread: &Thread) -> Result<ExternalSession> {
    // These primitive fields are Value in the generated SDK, so deserialize
    // the complete value rather than relying on private nested JSON keys.
    let cwd: String = serde_json::from_value(thread.cwd.clone())
        .context("Codex returned an invalid working directory")?;
    let name = thread
        .name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&thread.preview)
        .chars()
        .take(160)
        .collect::<String>();
    Ok(ExternalSession {
        source_id: thread.id.clone(),
        agent_type: shared::AgentType::Codex,
        name: if name.is_empty() {
            "Untitled Codex session".into()
        } else {
            name
        },
        working_directory: cwd,
        updated_at: thread.updated_at,
        model: thread.model.clone(),
        portal_session_id: None,
    })
}

pub async fn list(query: ExternalSessionQuery) -> Result<ExternalSessionList> {
    let mut client = client().await?;
    let response: ThreadListResponse = client
        .request(
            "thread/list",
            &ThreadListParams {
                cursor: query.cursor,
                limit: Some(40),
                archived: Some(false),
                cwd: query.working_directory.map(ThreadListCwdFilter::Variant0),
                search_term: query.search,
                sort_key: Some(ThreadSortKey::Updated_at),
                source_kinds: Some(vec![
                    ThreadSourceKind::Cli,
                    ThreadSourceKind::Vscode,
                    ThreadSourceKind::AppServer,
                ]),
                ..Default::default()
            },
        )
        .await?;
    let sessions = response
        .data
        .iter()
        .filter(|t| t.parent_thread_id.is_none() && !t.ephemeral)
        .map(summary)
        .collect::<Result<Vec<_>>>()?;
    Ok(ExternalSessionList {
        sessions,
        next_cursor: response.next_cursor,
    })
}

pub async fn prepare(source_id: &str, include_history: bool) -> Result<PreparedSessionImport> {
    let mut client = client().await?;
    let response: ThreadReadResponse = client
        .request(
            "thread/read",
            &ThreadReadParams {
                thread_id: source_id.into(),
                include_turns: Some(false),
            },
        )
        .await?;
    let thread = response.thread;
    if thread.ephemeral || thread.parent_thread_id.is_some() {
        bail!("Only saved root conversations can be imported");
    }
    if let Ok(ThreadStatus::Active { .. }) =
        serde_json::from_value::<ThreadStatus>(thread.status.clone())
    {
        bail!("This conversation is still running. Finish its turn and close the original client before importing.");
    }
    let session = summary(&thread)?;
    let mut messages = Vec::new();
    if include_history {
        let mut bytes = 0;
        if thread.history_mode == Some(ThreadHistoryMode::Paginated) {
            let mut cursor = None;
            let mut seen = HashSet::new();
            loop {
                let page = client.thread_turns_list(&ThreadTurnsListParams {
                    thread_id: source_id.into(), cursor, limit: Some(20),
                    items_view: Some(TurnItemsView::Full), sort_direction: Some(SortDirection::Asc),
                }).await.context("Codex could not read this history format. Update Codex or import with 'Copy conversation history' unchecked; native context is preserved.")?;
                append_turns(&mut messages, &mut bytes, page.data)?;
                cursor = page.next_cursor;
                match &cursor {
                    None => break,
                    Some(c) if !seen.insert(c.clone()) || seen.len() > 2500 => {
                        bail!("Codex returned a non-terminating history cursor")
                    }
                    _ => {}
                }
            }
        } else {
            let full: ThreadReadResponse = client
                .request(
                    "thread/read",
                    &ThreadReadParams {
                        thread_id: source_id.into(),
                        include_turns: Some(true),
                    },
                )
                .await?;
            append_turns(&mut messages, &mut bytes, full.thread.turns)?;
        }
        // Do not publish an inconsistent snapshot if another client changed it
        // while it was being paged. The user can retry after closing that client.
        let after: ThreadReadResponse = client
            .request(
                "thread/read",
                &ThreadReadParams {
                    thread_id: source_id.into(),
                    include_turns: Some(false),
                },
            )
            .await?;
        if after.thread.updated_at != thread.updated_at {
            bail!("The conversation changed during import. Close the original client and retry.");
        }
    }
    Ok(PreparedSessionImport { session, messages })
}

fn append_turns(
    messages: &mut Vec<ImportedMessage>,
    bytes: &mut usize,
    turns: Vec<Turn>,
) -> Result<()> {
    for turn in turns {
        if turn.status == codex_codes::TurnStatus::InProgress {
            bail!("A turn is still in progress. Finish it and close the original client before importing.");
        }
        for item in turn.items {
            let (role, content) = match item.payload {
                codex_codes::protocol::ThreadItemPayload::UserMessage { content, .. } => {
                    let text = content
                        .into_iter()
                        .map(input_text)
                        .collect::<Vec<_>>()
                        .join("\n");
                    ("user", serde_json::to_value(UserEchoEvent::new(text))?)
                }
                // Keep the wrapper: imported history must retain lifecycle times.
                payload => (
                    "assistant",
                    serde_json::to_value(ItemEvent::completed(serde_json::to_value(
                        codex_codes::protocol::ThreadItem { payload, ..item },
                    )?))?,
                ),
            };
            push_message(messages, bytes, role, content)?;
        }
        push_message(
            messages,
            bytes,
            "assistant",
            serde_json::to_value(TurnCompletedEvent::new(
                turn.id,
                crate::handler::turn_status_label(&turn.status).into(),
                turn.duration_ms,
                None,
            ))?,
        )?;
    }
    Ok(())
}

fn push_message(
    messages: &mut Vec<ImportedMessage>,
    bytes: &mut usize,
    role: &str,
    content: serde_json::Value,
) -> Result<()> {
    *bytes += serde_json::to_vec(&content)?.len();
    if *bytes > MAX_IMPORT_HISTORY_BYTES || messages.len() >= MAX_IMPORT_MESSAGES {
        bail!("This history exceeds Portal's import limit. Uncheck 'Copy conversation history' to import the full native context without copying scrollback.");
    }
    messages.push(ImportedMessage {
        role: role.into(),
        content,
    });
    Ok(())
}

fn input_text(input: UserInput) -> String {
    match input {
        UserInput::Text { text, .. } => text,
        UserInput::LocalImage { path, .. } => format!("[Local image: {path}]"),
        UserInput::Image { .. } => "[Image attachment retained in Codex]".into(),
        UserInput::LocalAudio { path } => format!("[Local audio: {path}]"),
        UserInput::Audio { .. } => "[Audio attachment retained in Codex]".into(),
        UserInput::Skill { name, .. } => format!("[Skill: {name}]"),
        UserInput::Mention { name, .. } => format!("[Mention: {name}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn turn(status: codex_codes::TurnStatus) -> Turn {
        Turn {
            id: "turn-1".into(),
            status,
            items: vec![
                codex_codes::protocol::ThreadItem::untimed(
                    codex_codes::protocol::ThreadItemPayload::UserMessage {
                        id: "user-1".into(),
                        client_id: None,
                        content: vec![UserInput::Text {
                            text: "Explain the design".into(),
                            text_elements: None,
                        }],
                    },
                ),
                codex_codes::protocol::ThreadItem::untimed(
                    codex_codes::protocol::ThreadItemPayload::AgentMessage {
                        id: "answer-1".into(),
                        text: "Saved answer".into(),
                        delivery: None,
                        memory_citation: None,
                        phase: None,
                        questions: None,
                    },
                ),
            ],
            started_at: None,
            completed_at: None,
            duration_ms: Some(42),
            error: None,
            items_view: None,
            root_turn_id: None,
        }
    }
    #[test]
    fn history_preserves_order_roles_and_terminal_status_without_usage() {
        let mut messages = vec![];
        let mut completed = turn(codex_codes::TurnStatus::Completed);
        completed.items[1].started_at_ms = Some(1_000);
        completed.items[1].completed_at_ms = Some(1_042);
        append_turns(&mut messages, &mut 0, vec![completed]).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content["content"], "Explain the design");
        assert_eq!(messages[1].content["item"]["text"], "Saved answer");
        assert_eq!(messages[1].content["item"]["startedAtMs"], 1_000);
        assert_eq!(messages[1].content["item"]["completedAtMs"], 1_042);
        assert_eq!(messages[2].content["status"], "completed");
        assert!(messages[2].content.get("usage").is_none());
    }
    #[test]
    fn active_turns_and_oversized_history_fail_without_truncation() {
        assert!(append_turns(
            &mut vec![],
            &mut 0,
            vec![turn(codex_codes::TurnStatus::InProgress)]
        )
        .is_err());
        let mut bytes = MAX_IMPORT_HISTORY_BYTES;
        assert!(append_turns(
            &mut vec![],
            &mut bytes,
            vec![turn(codex_codes::TurnStatus::Completed)]
        )
        .is_err());
    }
}

#[cfg(test)]
mod live_tests {
    /// Explicit read-only smoke test, using a caller-selected local thread.
    /// Never calls thread/resume or turn/start and prints no conversation data.
    #[tokio::test]
    #[ignore = "requires a local Codex CLI and PORTAL_IMPORT_TEST_THREAD_ID"]
    async fn native_thread_is_readable_without_resuming() {
        let id = std::env::var("PORTAL_IMPORT_TEST_THREAD_ID").expect("select a saved test thread");
        let include_history =
            std::env::var("PORTAL_IMPORT_TEST_COPY_HISTORY").is_ok_and(|v| v == "1");
        let snapshot = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            super::prepare(&id, include_history),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(snapshot.session.source_id, id);
        if !include_history {
            assert!(snapshot.messages.is_empty());
        }
        let listed = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            super::list(shared::api::ExternalSessionQuery {
                working_directory: Some(snapshot.session.working_directory),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!listed.sessions.is_empty());
    }
}
