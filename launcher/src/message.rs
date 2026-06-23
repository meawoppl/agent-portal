//! `agent-portal message` subcommands: list your sessions and send a message
//! into one. A thin client over the backend's `/api/agent/*` endpoints,
//! authenticated with the launcher's stored proxy token (`launcher.json`) — so
//! an agent can just shell out to `agent-portal message send …` with no extra
//! credentials. The message is delivered to the target session's agent as an
//! input turn, attributed with the sender's session id.

use anyhow::{anyhow, Context, Result};

use shared::api::{AgentSessionsResponse, SendAgentMessageRequest, SendAgentMessageResponse};

/// The calling agent's own portal session id, read from whatever the agent
/// already exposes — no portal-side injection needed:
///
/// - Claude Code sets `CLAUDE_CODE_SESSION_ID` to the id we spawn it with
///   (`--session-id <portal id>`), so it already *is* the portal session id.
/// - Codex sets `CODEX_THREAD_ID`, which is *not* the portal id, so we reverse
///   it through the launcher's `codex_threads.json` map to the portal session.
/// - `PORTAL_SESSION_ID` is honored as a manual/explicit override.
///
/// Returns `None` when none apply (e.g. a human shell), in which case the
/// recipient falls back to user attribution.
fn sender_session_id() -> Option<String> {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());

    if let Some(id) = env("CLAUDE_CODE_SESSION_ID").or_else(|| env("PORTAL_SESSION_ID")) {
        return Some(id);
    }
    if let Some(thread_id) = env("CODEX_THREAD_ID") {
        return crate::process_manager::session_id_for_codex_thread(&thread_id)
            .map(|id| id.to_string());
    }
    None
}

/// Resolve the HTTP API base URL and auth token from the launcher config.
fn api_base() -> Result<(String, String)> {
    let config = crate::config::load_config();
    let token = config
        .auth_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("Not authenticated — run `agent-portal login` first"))?;
    let ws_url = config
        .backend_url
        .unwrap_or_else(|| shared::default_backend_url().to_string());
    // The config stores the WebSocket URL; the HTTP API shares the host.
    let http = ws_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    Ok((http.trim_end_matches('/').to_string(), token))
}

/// `agent-portal message list` — print the caller's sessions (agents).
pub async fn list() -> Result<()> {
    let (base, token) = api_base()?;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/agent/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .context("request to backend failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("backend returned {}", resp.status()));
    }
    let data: AgentSessionsResponse = resp.json().await.context("malformed response")?;
    if data.sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    let self_id = sender_session_id();
    for s in &data.sessions {
        let marker = if self_id.as_deref() == Some(&s.id.to_string()) {
            " (this session)"
        } else {
            ""
        };
        println!(
            "{}  {}  [{}/{}]  {}{}",
            s.id, s.session_name, s.agent_type, s.status, s.working_directory, marker
        );
    }
    Ok(())
}

/// `agent-portal message send <agent-id> <message>` — deliver a message into a
/// session as an input turn.
pub async fn send(agent_id: &str, message: &str) -> Result<()> {
    let (base, token) = api_base()?;
    let from = sender_session_id();
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/agent/sessions/{agent_id}/message"))
        .bearer_auth(&token)
        .json(&SendAgentMessageRequest {
            message: message.to_string(),
            from,
        })
        .send()
        .await
        .context("request to backend failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("backend returned {}: {}", status, body.trim()));
    }
    let data: SendAgentMessageResponse = resp.json().await.context("malformed response")?;
    if data.delivered {
        println!("Delivered (seq {}).", data.seq);
    } else {
        println!("Queued for the session's reconnect (seq {}).", data.seq);
    }
    Ok(())
}
