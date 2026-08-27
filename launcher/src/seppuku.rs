//! Self-termination command for an agent session.
//!
//! There is deliberately no target argument: the session id is resolved from
//! the environment injected into this agent process, then the normal backend
//! stop lifecycle disconnects the proxy and asks its launcher to stop it.

use anyhow::{Context, Result};

pub async fn run() -> Result<()> {
    let (base, token) = crate::message::api_base()?;
    let client = reqwest::Client::new();
    let session_id = crate::message::current_session_id(&client, &base, &token).await?;

    println!("Terminating this portal session ({session_id})…");
    let response = client
        .post(format!("{base}/api/sessions/{session_id}/stop"))
        .bearer_auth(token)
        .send()
        .await
        .context("could not reach the portal backend")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("backend returned {status}: {}", body.trim());
    }

    println!("Session termination requested.");
    Ok(())
}
