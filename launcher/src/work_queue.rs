//! `agent-portal work-queue` subcommands for durable edit-stack items.
//!
//! The browser's annotation tool and forwarded plugin surfaces call this the
//! edit stack internally. The CLI presents the same backing store as a work
//! queue so agents can inspect pending visual/text tasks and add focused work
//! items to another session without immediately interrupting it.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use shared::api::{
    AgentSessionInfo, CreateEditStackRequest, EditStackItem, EditStackItemInput, EditStackResponse,
};
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueueSnapshot {
    session: AgentSessionInfo,
    items: Vec<EditStackItem>,
}

pub async fn list(agent_id: Option<&str>, all: bool, json: bool) -> Result<()> {
    let (base, token) = crate::message::api_base()?;
    let client = reqwest::Client::new();
    let sessions = crate::message::fetch_sessions(&client, &base, &token).await?;

    if let Some(agent_id) = agent_id {
        let resolved = crate::message::resolve_session_id(agent_id, &sessions.sessions)?;
        let session = sessions
            .sessions
            .iter()
            .find(|session| session.id == resolved)
            .ok_or_else(|| anyhow!("resolved session disappeared from the listing"))?;
        let response = fetch_queue(&client, &base, &token, resolved).await?;
        let items = filter_items(response.items, all);
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&QueueSnapshot {
                    session: session.clone(),
                    items,
                })?
            );
            return Ok(());
        }
        print_session_header(session, &sessions.sessions);
        print_items(&items, all);
        return Ok(());
    }

    let mut snapshots = Vec::new();
    for session in &sessions.sessions {
        let response = fetch_queue(&client, &base, &token, session.id).await?;
        let items = filter_items(response.items, all);
        if all || !items.is_empty() {
            snapshots.push(QueueSnapshot {
                session: session.clone(),
                items,
            });
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&snapshots)?);
        return Ok(());
    }

    if snapshots.is_empty() {
        println!(
            "No {}work queue items found.",
            if all { "" } else { "pending " }
        );
        return Ok(());
    }

    for snapshot in &snapshots {
        let pending = snapshot
            .items
            .iter()
            .filter(|item| item.status == "pending")
            .count();
        println!(
            "{}  {} / {} / {}  {}  {}  {}  {} pending / {} shown",
            crate::message::display_session_id(&snapshot.session, &sessions.sessions),
            crate::message::full_agent_name(&snapshot.session),
            if crate::message::session_is_connected(&snapshot.session) {
                "connected"
            } else {
                "disconnected"
            },
            crate::message::session_activity_label(&snapshot.session),
            snapshot.session.hostname,
            snapshot.session.session_name,
            snapshot.session.working_directory,
            pending,
            snapshot.items.len(),
        );
    }
    Ok(())
}

pub async fn add(
    agent_id: &str,
    message: &str,
    title: Option<&str>,
    context: Option<&str>,
    source: Option<&str>,
) -> Result<()> {
    let body = message.trim();
    if body.is_empty() {
        return Err(anyhow!("work queue item body cannot be empty"));
    }

    let (base, token) = crate::message::api_base()?;
    let client = reqwest::Client::new();
    let sessions = crate::message::fetch_sessions(&client, &base, &token).await?;
    let resolved = crate::message::resolve_session_id(agent_id, &sessions.sessions)?;
    let default_source = default_source(&sessions.sessions);
    let request = CreateEditStackRequest {
        source: parse_loose_json(source).or(default_source),
        items: vec![EditStackItemInput {
            title: Some(
                shared::strings::trimmed_non_blank(title)
                    .map(str::to_string)
                    .unwrap_or_else(|| default_title(body)),
            ),
            body: Some(body.to_string()),
            context: parse_loose_json(context),
            ..Default::default()
        }],
    };

    let response = crate::message::request_with_retry(false, || {
        client
            .post(format!("{base}/api/agent/sessions/{resolved}/edit-stack"))
            .bearer_auth(&token)
            .json(&request)
            .send()
    })
    .await?;
    let data: EditStackResponse = response.json().await.context("malformed response")?;
    let pending = data
        .items
        .iter()
        .filter(|item| item.status == "pending")
        .count();
    println!(
        "Queued work item for {} ({} pending).",
        shared::short_uuid(&resolved),
        pending
    );
    Ok(())
}

async fn fetch_queue(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    session_id: Uuid,
) -> Result<EditStackResponse> {
    let response = crate::message::request_with_retry(true, || {
        client
            .get(format!("{base}/api/agent/sessions/{session_id}/edit-stack"))
            .bearer_auth(token)
            .send()
    })
    .await?;
    response.json().await.context("malformed response")
}

fn filter_items(items: Vec<EditStackItem>, all: bool) -> Vec<EditStackItem> {
    if all {
        items
    } else {
        items
            .into_iter()
            .filter(|item| item.status == "pending")
            .collect()
    }
}

fn print_session_header(session: &AgentSessionInfo, sessions: &[AgentSessionInfo]) {
    println!(
        "{}  {} / {} / {}  {}  {}  {}",
        crate::message::display_session_id(session, sessions),
        crate::message::full_agent_name(session),
        if crate::message::session_is_connected(session) {
            "connected"
        } else {
            "disconnected"
        },
        crate::message::session_activity_label(session),
        session.hostname,
        session.session_name,
        session.working_directory,
    );
}

fn print_items(items: &[EditStackItem], all: bool) {
    if items.is_empty() {
        println!("No {}work queue items.", if all { "" } else { "pending " });
        return;
    }

    println!(
        "{} {} item{}:",
        items.len(),
        if all { "work queue" } else { "pending" },
        if items.len() == 1 { "" } else { "s" },
    );
    for item in items {
        let creator = item
            .created_by_name
            .as_deref()
            .filter(|name| shared::strings::is_non_empty(name))
            .unwrap_or("portal");
        println!(
            "  {}  [{}] {}  by {}  {}",
            shared::short_uuid(&item.id),
            item.status,
            item.title,
            creator,
            age_label(&item.created_at),
        );
        if shared::strings::is_non_blank(&item.body) {
            println!("      {}", one_line_preview(&item.body, 140));
        }
        if item.image_data_url.is_some() {
            println!("      image: yes");
        }
        if let Some(context) = item.context.as_ref() {
            println!("      context: {}", json_preview(context, 140));
        }
    }
}

fn default_source(sessions: &[AgentSessionInfo]) -> Option<serde_json::Value> {
    let sender = match crate::message::sender_session_id(sessions) {
        Ok(sender) => sender,
        Err(error) => {
            eprintln!("warning: could not identify sender session: {error}");
            None
        }
    };
    let mut source = serde_json::Map::new();
    source.insert(
        "kind".to_string(),
        serde_json::Value::String("agent-portal-cli".to_string()),
    );
    if let Some(sender) = sender {
        source.insert(
            "senderSessionId".to_string(),
            serde_json::Value::String(sender),
        );
    }
    Some(serde_json::Value::Object(source))
}

fn parse_loose_json(value: Option<&str>) -> Option<serde_json::Value> {
    let raw = shared::strings::trimmed_non_blank(value)?;
    Some(serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string())))
}

fn default_title(body: &str) -> String {
    let first_line = body
        .lines()
        .find(|line| shared::strings::is_non_blank(line))
        .unwrap_or(body);
    let title = one_line_preview(first_line, 80);
    if title.is_empty() {
        "CLI work item".to_string()
    } else {
        title
    }
}

fn one_line_preview(value: &str, max_chars: usize) -> String {
    let collapsed = shared::strings::collapse_whitespace(value);
    let mut output = collapsed.chars().take(max_chars).collect::<String>();
    if collapsed.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn json_preview(value: &serde_json::Value, max_chars: usize) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    one_line_preview(&raw, max_chars)
}

fn age_label(timestamp: &str) -> String {
    if let Ok(then) = chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.f") {
        let then = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(then, chrono::Utc);
        let now = chrono::Utc::now();
        let secs = (now - then).num_seconds().max(0);
        return match secs {
            0..=59 => format!("{secs}s"),
            60..=3599 => format!("{}m", secs / 60),
            3600..=86399 => format!("{}h", secs / 3600),
            _ => format!("{}d", secs / 86400),
        };
    }
    crate::message::relative_age(timestamp, chrono::Utc::now())
}
