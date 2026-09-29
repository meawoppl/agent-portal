//! `agent-portal progress`: drive a live progress bar in the calling session's
//! web view. A thin client over the backend's
//! `POST /api/agent/sessions/{id}/progress`, authenticated and attributed the
//! same way as `agent-portal show` — so a shell loop can just call it with the
//! latest number and needs no credentials.

use anyhow::{anyhow, Context, Result};

use shared::api::{parse_progress_fraction, AgentProgressRequest, DEFAULT_PROGRESS_ID};

/// `agent-portal progress [VALUE] [--label L] [--max N] [--id ID] [--done]`.
///
/// No `value` and no `done` makes an indeterminate bar; `done` removes it.
pub async fn run(
    value: Option<&str>,
    label: Option<&str>,
    max: f64,
    id: Option<&str>,
    done: bool,
) -> Result<()> {
    let fraction = value
        .map(|v| parse_progress_fraction(v, max))
        .transpose()
        .map_err(|e| anyhow!(e))?;
    let request = AgentProgressRequest {
        id: id.unwrap_or(DEFAULT_PROGRESS_ID).to_string(),
        label: label.map(str::to_string),
        fraction,
        clear: done,
    };

    let client = reqwest::Client::new();
    let (base, token) = crate::message::api_base()?;
    let session_id = crate::message::current_session_id(&client, &base, &token)
        .await
        .context("could not determine the calling session for `agent-portal progress`")?;

    let resp = client
        .post(format!("{base}/api/agent/sessions/{session_id}/progress"))
        .bearer_auth(&token)
        .json(&request)
        .send()
        .await
        .context("request to backend failed")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(crate::message::backend_http_error(status, &body));
    }
    Ok(())
}
