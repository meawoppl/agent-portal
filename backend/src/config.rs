//! Server bootstrap configuration: environment parsing and OAuth client setup.

use oauth2::{
    basic::BasicClient, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, RedirectUrl,
    TokenUrl,
};
use std::env;
use tower_cookies::Key;

use crate::handlers;

pub type GoogleOAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Build the Google OAuth client from environment variables.
/// Returns `None` in dev mode (OAuth is bypassed).
pub fn build_google_oauth_client(dev_mode: bool) -> anyhow::Result<Option<GoogleOAuthClient>> {
    if dev_mode {
        return Ok(None);
    }

    let client_id =
        ClientId::new(env::var("GOOGLE_CLIENT_ID").expect("GOOGLE_CLIENT_ID must be set"));
    let client_secret = ClientSecret::new(
        env::var("GOOGLE_CLIENT_SECRET").expect("GOOGLE_CLIENT_SECRET must be set"),
    );
    let auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())?;
    let token_url = TokenUrl::new("https://oauth2.googleapis.com/token".to_string())?;
    let redirect_uri = RedirectUrl::new(
        env::var("GOOGLE_REDIRECT_URI").expect("GOOGLE_REDIRECT_URI must be set"),
    )?;

    Ok(Some(
        BasicClient::new(client_id)
            .set_client_secret(client_secret)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_uri),
    ))
}

/// Server configuration parsed from environment variables.
pub struct ServerConfig {
    pub host: String,
    pub port: String,
    pub public_url: String,
    pub cookie_key: Key,
    pub jwt_secret: String,
    pub app_title: String,
    pub splash_text: Option<String>,
    pub allowed_email_domain: Option<String>,
    pub allowed_emails: Option<Vec<String>>,
    pub message_retention_count: i64,
    pub message_retention_days: u32,
    pub session_max_age_days: u32,
    pub max_image_mb: u32,
    pub image_store_max_bytes: u64,
    pub image_store_ttl: std::time::Duration,
}

impl ServerConfig {
    pub fn from_env(dev_mode: bool) -> Self {
        // Get base URL from env or construct from host/port
        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
        let public_url = env::var("BASE_URL").unwrap_or_else(|_| {
            // Default to localhost for development
            format!("http://localhost:{}", port)
        });

        // SESSION_SECRET backs both the signed-cookie key and the proxy/launcher
        // JWT secret. Set it to a stable 64+ byte value in production so cookies
        // and tokens survive a redeploy. When it is absent we generate a random
        // ephemeral secret so the server still boots — at the cost of invalidating
        // every cookie and token on restart. (It must never be a hard-coded
        // constant: a known secret lets anyone forge tokens for any user.)
        let session_secret = env::var("SESSION_SECRET").ok().unwrap_or_else(|| {
            tracing::warn!(
                "SESSION_SECRET is not set — generating a random ephemeral secret. \
                 All signed cookies and proxy/launcher JWTs will be invalidated on \
                 restart; set SESSION_SECRET to a stable 64+ byte value in production."
            );
            hex::encode(Key::generate().master())
        });
        let cookie_key = {
            let bytes = session_secret.as_bytes();
            if bytes.len() < 64 {
                tracing::warn!("SESSION_SECRET should be at least 64 bytes, padding with zeros");
                let mut padded = vec![0u8; 64];
                padded[..bytes.len()].copy_from_slice(bytes);
                Key::from(&padded)
            } else {
                Key::from(&bytes[..64])
            }
        };

        // JWT secret for proxy tokens — same source as the cookie key above.
        let jwt_secret = session_secret;

        // App title (customizable via environment variable)
        // In dev mode, override with a warning to make it obvious
        let app_title = if dev_mode {
            "⚠️ INSECURE DEV MODE ⚠️".to_string()
        } else {
            env::var("APP_TITLE").unwrap_or_else(|_| "Agent Portal".to_string())
        };

        let splash_text = env::var("SPLASH_TEXT").ok();

        // Email access control (optional)
        let allowed_email_domain = env::var("ALLOWED_EMAIL_DOMAIN").ok();
        let allowed_emails = env::var("ALLOWED_EMAILS").ok().map(|s| {
            s.split(',')
                .map(|e| e.trim().to_lowercase())
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
        });

        if allowed_email_domain.is_some() || allowed_emails.is_some() {
            tracing::info!(
                "Email access control enabled: domain={:?}, specific_emails={}",
                allowed_email_domain,
                allowed_emails.as_ref().map(|e| e.len()).unwrap_or(0)
            );
        }

        // Message retention settings
        let message_retention_count: i64 = env::var("MESSAGE_RETENTION_COUNT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);
        let message_retention_days: u32 = env::var("MESSAGE_RETENTION_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        let session_max_age_days: u32 = env::var("SESSION_MAX_AGE_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(14);

        let max_image_mb: u32 = env::var("PORTAL_MAX_IMAGE_MB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);

        // Image store eviction caps — both required to bound memory on long
        // image-heavy sessions (see issue #787). Defaults are 256 MiB / 1 h.
        let image_store_max_mb: u64 = env::var("PORTAL_IMAGE_STORE_MAX_MB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(handlers::images::DEFAULT_IMAGE_STORE_MAX_BYTES / (1024 * 1024));
        let image_store_ttl_secs: u64 = env::var("PORTAL_IMAGE_STORE_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(handlers::images::DEFAULT_IMAGE_STORE_TTL.as_secs());
        let image_store_max_bytes = image_store_max_mb.saturating_mul(1024 * 1024);
        let image_store_ttl = std::time::Duration::from_secs(image_store_ttl_secs);

        tracing::info!(
            "Message retention: max {} messages/session, {} days",
            message_retention_count,
            message_retention_days
        );
        tracing::info!(
            "Session max age: {} days (0 = disabled)",
            session_max_age_days
        );
        tracing::info!("Max image size: {} MB", max_image_mb);
        tracing::info!(
            "Image store cap: {} MB total, {}s TTL per entry",
            image_store_max_mb,
            image_store_ttl_secs
        );

        ServerConfig {
            host,
            port,
            public_url,
            cookie_key,
            jwt_secret,
            app_title,
            splash_text,
            allowed_email_domain,
            allowed_emails,
            message_retention_count,
            message_retention_days,
            session_max_age_days,
            max_image_mb,
            image_store_max_bytes,
            image_store_ttl,
        }
    }
}
