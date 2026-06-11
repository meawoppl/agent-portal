use anyhow::{Context, Result};
use colored::Colorize;
use shared::api::{DeviceCodeRequest, DeviceCodeResponse, DeviceFlowPollRequest};
use shared::DevicePollResponse;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;

/// Result of a successful device flow login.
pub struct DeviceFlowResult {
    pub access_token: String,
    pub user_id: String,
    pub user_email: String,
}

/// Convert a WebSocket URL to an HTTP URL for API calls.
pub fn ws_to_http(url: &str) -> String {
    url.replace("ws://", "http://")
        .replace("wss://", "https://")
}

/// Run the OAuth device flow against a portal backend.
///
/// Requests a device code, displays a verification URL to the user,
/// and polls until the user approves (or the code expires).
pub async fn device_flow_login(
    backend_url: &str,
    working_directory: Option<&str>,
) -> Result<DeviceFlowResult> {
    let client = reqwest::Client::new();

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let auth_base = ws_to_http(backend_url);
    let device_code_url = format!("{}/api/auth/device/code", auth_base);

    info!("Requesting device code from {}", device_code_url);

    let body = DeviceCodeRequest {
        hostname: Some(hostname),
        working_directory: working_directory.map(|s| s.to_string()),
    };
    let http_response = client
        .post(&device_code_url)
        .json(&body)
        .send()
        .await
        .context("Failed to request device code")?;

    let status = http_response.status();
    if !status.is_success() {
        match status.as_u16() {
            503 => {
                anyhow::bail!(
                    "Device flow authentication is not available on this server.\n\
                     \n\
                     This usually means:\n\
                     - The server is running in dev mode, or\n\
                     - OAuth is not configured on the server\n\
                     \n\
                     Try using the web UI to generate a setup token instead."
                );
            }
            401 => anyhow::bail!("Authentication required. Please check your credentials."),
            404 => anyhow::bail!("Device flow endpoint not found. Server may be outdated."),
            _ => {
                let body = http_response.text().await.unwrap_or_default();
                anyhow::bail!("Server returned error {}: {}", status, body);
            }
        }
    }

    let response: DeviceCodeResponse = http_response
        .json()
        .await
        .context("Failed to parse device code response")?;

    let full_url = format!(
        "{}?user_code={}",
        response.verification_uri, response.user_code
    );
    println!();
    println!(
        "{}",
        "╔═══════════════════════════════════════════════════════╗".bright_blue()
    );
    println!(
        "{}",
        "║           🔐 Authentication Required                 ║".bright_blue()
    );
    println!(
        "{}",
        "╚═══════════════════════════════════════════════════════╝".bright_blue()
    );
    println!();
    println!("  To authenticate this machine, visit:");
    println!();
    println!("    {}", full_url.bright_green().bold());
    println!();
    println!(
        "  {} Code: {}",
        "📋".bright_cyan(),
        response.user_code.bright_yellow().bold()
    );
    println!();
    println!("  {} Waiting for authentication...", "⏳".bright_cyan());
    println!();

    let poll_url = format!("{}/api/auth/device/poll", auth_base);
    let interval = Duration::from_secs(response.interval.max(5));
    let expires_at = std::time::Instant::now() + Duration::from_secs(response.expires_in);

    loop {
        if std::time::Instant::now() > expires_at {
            anyhow::bail!("Authentication timed out");
        }

        sleep(interval).await;

        let poll_body = DeviceFlowPollRequest {
            device_code: response.device_code.clone(),
        };
        let poll_http_response = client
            .post(&poll_url)
            .json(&poll_body)
            .send()
            .await
            .context("Failed to poll for authentication")?;

        if !poll_http_response.status().is_success() {
            let status = poll_http_response.status();
            let body = poll_http_response.text().await.unwrap_or_default();
            anyhow::bail!("Poll request failed with status {}: {}", status, body);
        }

        let poll_response: DevicePollResponse = poll_http_response
            .json()
            .await
            .context("Failed to parse poll response")?;

        match poll_response {
            DevicePollResponse::Pending => continue,
            DevicePollResponse::Complete {
                access_token,
                user_id,
                user_email,
            } => {
                println!();
                println!("  {} Authentication successful!", "✓".bright_green());
                println!("  Logged in as: {}", user_email.bright_cyan());
                println!();
                return Ok(DeviceFlowResult {
                    access_token,
                    user_id,
                    user_email,
                });
            }
            DevicePollResponse::Expired => {
                anyhow::bail!("Authentication code expired");
            }
            DevicePollResponse::Denied => {
                anyhow::bail!("Authentication was denied");
            }
        }
    }
}
