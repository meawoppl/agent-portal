//! `agent-portal message` subcommands: list your sessions and send a message
//! into one. A thin client over the backend's `/api/agent/*` endpoints,
//! authenticated with the launcher's stored proxy token (`launcher.json`) — so
//! an agent can just shell out to `agent-portal message send …` with no extra
//! credentials. The message is delivered to the target session's agent as an
//! input turn, attributed with the sender's session id.

use anyhow::{anyhow, Context, Result};

use shared::api::{AgentSessionsResponse, SendAgentMessageRequest, SendAgentMessageResponse};

const SHORT_SESSION_ID_LEN: usize = 8;

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
    let client = reqwest::Client::new();
    let data = fetch_sessions(&client, &base, &token).await?;
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
            display_session_id(s, &data.sessions),
            s.session_name,
            s.agent_type,
            s.status,
            s.working_directory,
            marker
        );
    }
    Ok(())
}

async fn fetch_sessions(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<AgentSessionsResponse> {
    let resp = client
        .get(format!("{base}/api/agent/sessions"))
        .bearer_auth(token)
        .send()
        .await
        .context("request to backend failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("backend returned {}", resp.status()));
    }
    resp.json().await.context("malformed response")
}

/// `agent-portal message send <agent-id> <message>` — deliver a message into a
/// session as an input turn.
pub async fn send(agent_id: &str, message: &str) -> Result<()> {
    let (base, token) = api_base()?;
    let client = reqwest::Client::new();
    let sessions = fetch_sessions(&client, &base, &token).await?;
    let resolved_agent_id = resolve_session_id(agent_id, &sessions.sessions)?;
    let from = sender_session_id();
    let resp = client
        .post(format!(
            "{base}/api/agent/sessions/{resolved_agent_id}/message"
        ))
        .bearer_auth(token)
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

fn short_session_id(id: &uuid::Uuid) -> String {
    id.simple()
        .to_string()
        .chars()
        .take(SHORT_SESSION_ID_LEN)
        .collect()
}

fn display_session_id(
    session: &shared::api::AgentSessionInfo,
    sessions: &[shared::api::AgentSessionInfo],
) -> String {
    let short = short_session_id(&session.id);
    let collision = sessions
        .iter()
        .filter(|candidate| short_session_id(&candidate.id) == short)
        .count()
        > 1;
    if collision {
        session.id.to_string()
    } else {
        short
    }
}

fn resolve_session_id(
    input: &str,
    sessions: &[shared::api::AgentSessionInfo],
) -> Result<uuid::Uuid> {
    let prefix = normalize_session_id_prefix(input)?;
    let matches = sessions
        .iter()
        .filter(|session| session.id.simple().to_string().starts_with(&prefix))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [session] => Ok(session.id),
        [] => Err(anyhow!("no session id matches `{}`", input.trim())),
        matches => {
            let ids = matches
                .iter()
                .map(|session| session.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "session id prefix `{}` is ambiguous; use more characters or a full id (matches: {})",
                input.trim(),
                ids
            ))
        }
    }
}

fn normalize_session_id_prefix(input: &str) -> Result<String> {
    let prefix = input.trim().replace('-', "").to_ascii_lowercase();
    if prefix.is_empty() {
        return Err(anyhow!("session id prefix cannot be empty"));
    }
    if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "session id prefix `{}` must contain only hex digits",
            input.trim()
        ));
    }
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::api::AgentSessionInfo;
    use uuid::Uuid;

    fn session(id: &str) -> AgentSessionInfo {
        AgentSessionInfo {
            id: Uuid::parse_str(id).expect("valid uuid"),
            session_name: "session".to_string(),
            working_directory: "/repo".to_string(),
            agent_type: "codex".to_string(),
            status: "active".to_string(),
            hostname: "host".to_string(),
        }
    }

    #[test]
    fn display_session_id_uses_short_prefix_without_collision() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("abcdef12-0000-0000-0000-000000000000"),
        ];

        assert_eq!(display_session_id(&sessions[0], &sessions), "12345678");
    }

    #[test]
    fn display_session_id_uses_full_uuid_for_short_prefix_collision() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("12345678-ffff-0000-0000-000000000000"),
        ];

        assert_eq!(
            display_session_id(&sessions[0], &sessions),
            "12345678-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn resolve_session_id_accepts_unique_short_prefix() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("abcdef12-0000-0000-0000-000000000000"),
        ];

        assert_eq!(
            resolve_session_id("12345678", &sessions).expect("resolved"),
            sessions[0].id
        );
    }

    #[test]
    fn resolve_session_id_accepts_full_uuid() {
        let sessions = vec![session("12345678-0000-0000-0000-000000000000")];

        assert_eq!(
            resolve_session_id("12345678-0000-0000-0000-000000000000", &sessions)
                .expect("resolved"),
            sessions[0].id
        );
    }

    #[test]
    fn resolve_session_id_rejects_ambiguous_prefix() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("12345678-ffff-0000-0000-000000000000"),
        ];

        let err = resolve_session_id("12345678", &sessions)
            .expect_err("ambiguous prefix should fail")
            .to_string();
        assert!(err.contains("ambiguous"));
        assert!(err.contains("12345678-0000-0000-0000-000000000000"));
        assert!(err.contains("12345678-ffff-0000-0000-000000000000"));
    }
}
