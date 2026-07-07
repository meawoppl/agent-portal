//! `agent-portal forward` subcommands: expose a local HTTP port through the
//! portal (docs/PORT_FORWARDING.md). A thin client over the backend's
//! `/api/agent/sessions/{id}/forwards` endpoints, authenticated with the
//! launcher's stored proxy token — so an agent can shell out to
//! `agent-portal forward <port>` with no extra credentials and paste the
//! printed URL for the user.

use anyhow::{anyhow, Context, Result};

use shared::api::{CreateForwardRequest, CreateForwardResponse, SessionForwardsResponse};

/// Resolve the API base URL and auth token from launcher config (shared with
/// `message`).
fn api_base() -> Result<(String, String)> {
    let config = crate::config::load_config();
    let token = config
        .auth_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("Not authenticated — run `agent-portal login` first"))?;
    let ws_url = config
        .backend_url
        .unwrap_or_else(|| shared::default_backend_url().to_string());
    let http = ws_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    Ok((http.trim_end_matches('/').to_string(), token))
}

/// The calling agent's own portal session id (reuses `message`'s resolver, so
/// Claude / Codex / explicit-override all work).
fn session_id() -> Result<String> {
    crate::message::sender_session_id().ok_or_else(|| {
        anyhow!("run this from inside an agent session (no portal session id found)")
    })
}

/// `agent-portal forward <port>` — register a forward and print its URL.
pub async fn open(port: u16) -> Result<()> {
    if port == 0 {
        return Err(anyhow!("port must be 1-65535"));
    }
    let (base, token) = api_base()?;
    let session = session_id()?;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/agent/sessions/{session}/forwards"))
        .bearer_auth(&token)
        .json(&CreateForwardRequest { port })
        .send()
        .await
        .context("request to backend failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("backend returned {}: {}", status, body.trim()));
    }
    let data: CreateForwardResponse = resp.json().await.context("malformed response")?;

    // Exactly one URL line, so agents can relay it verbatim.
    println!("{}", data.forward.url);
    match data.listening {
        Some(false) => eprintln!(
            "warning: nothing is listening on port {} yet{}",
            port,
            data.probe_error
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ),
        None => eprintln!(
            "warning: could not confirm the port is live (agent proxy offline or too old)"
        ),
        Some(true) => {}
    }
    Ok(())
}

/// `agent-portal forward list` — active forwards for this session.
pub async fn list() -> Result<()> {
    let (base, token) = api_base()?;
    let session = session_id()?;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/agent/sessions/{session}/forwards"))
        .bearer_auth(&token)
        .send()
        .await
        .context("request to backend failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("backend returned {}", resp.status()));
    }
    let data: SessionForwardsResponse = resp.json().await.context("malformed response")?;
    if data.forwards.is_empty() {
        println!("No active forwards.");
        return Ok(());
    }
    for f in &data.forwards {
        println!(":{}  {}", f.port, f.url);
    }
    Ok(())
}

/// `agent-portal forward close <port>` — revoke a forward.
pub async fn close(port: u16) -> Result<()> {
    let (base, token) = api_base()?;
    let session = session_id()?;
    let client = reqwest::Client::new();

    let resp = client
        .delete(format!(
            "{base}/api/agent/sessions/{session}/forwards/{port}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .context("request to backend failed")?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(anyhow!("no forward on port {}", port));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("backend returned {}: {}", status, body.trim()));
    }
    println!("Closed forward on port {}.", port);
    Ok(())
}
