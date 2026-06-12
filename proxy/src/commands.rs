//! Subcommand handlers for logout and init.

use anyhow::Result;

use crate::config::{ProxyConfig, SessionAuth};
use crate::ui;
use crate::util;

/// Handle the --logout command
pub fn handle_logout(config: &mut ProxyConfig, cwd: &str) -> Result<()> {
    if let Some(removed) = config.remove_session_auth(cwd) {
        config.atomic_save()?;
        ui::print_logout_success(&removed.user_email.unwrap_or_default());
    } else {
        ui::print_no_cached_auth();
    }
    Ok(())
}

/// Handle the --init command
pub fn handle_init(
    config: &mut ProxyConfig,
    cwd: &str,
    init_value: &str,
    backend_url_override: Option<&str>,
) -> Result<()> {
    let (parsed_backend_url, token) = util::parse_init_value(init_value)?;

    // Resolve backend URL: CLI override > parsed from init value (required)
    let backend_url = backend_url_override
        .map(|s| s.to_string())
        .or(parsed_backend_url)
        .ok_or_else(|| {
            anyhow::anyhow!("No backend URL found in init value. Specify --backend-url explicitly.")
        })?;

    // Extract user info from JWT (basic parsing without verification)
    let user_email = util::extract_email_from_jwt(&token);

    ui::print_init_start(user_email.as_deref().unwrap_or("unknown user"));

    // Save to config
    config.set_session_auth(
        cwd.to_string(),
        SessionAuth {
            auth_token: token,
            user_email: user_email.clone(),
            last_used: chrono::Utc::now().to_rfc3339(),
            backend_url: Some(backend_url.clone()),
        },
    );

    // Save the backend URL to directory config
    config.set_backend_url(cwd, &backend_url);

    // Also set as the global default so it works in other directories
    config.preferences.default_backend_url = Some(backend_url.clone());

    config.atomic_save()?;

    ui::print_init_complete(
        &user_email.unwrap_or_else(|| "this directory".to_string()),
        &backend_url,
    );

    Ok(())
}
