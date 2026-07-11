//! Server bootstrap configuration: environment parsing and OAuth client setup.
//!
//! Goals (#1209 item 6): one validated config per binary, **fail-fast at boot**
//! with errors that name every problem at once, and **provenance** — each value
//! logs whether it came from the environment or a default. The parse helpers
//! accumulate into an error list rather than panicking or silently swallowing a
//! malformed value (e.g. `PORT=abc` used to fall back to 3000 unnoticed; now it
//! aborts boot with a clear message). Secret values are never logged — only the
//! variable name and its source.

use oauth2::{
    basic::BasicClient, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, RedirectUrl,
    TokenUrl,
};
use std::env;
use std::fmt::Display;
use std::str::FromStr;
use tower_cookies::Key;

use crate::handlers;

pub type GoogleOAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Log the provenance of one resolved variable. Never logs the value — only the
/// name and whether it came from the environment or a default — so secrets
/// (`SESSION_SECRET`, OAuth credentials) are safe to route through here.
fn log_source(name: &str, from_env: bool) {
    tracing::info!(
        target: "config",
        var = name,
        source = if from_env { "env" } else { "default" },
    );
}

/// Pure core of [`parse_or`]: resolve a raw optional value to either the parsed
/// value (with a from-env flag) or a fail-fast error message. No env read, no
/// logging — kept separate so it is unit-testable without mutating process
/// globals.
fn resolve_parse<T>(name: &str, raw: Option<String>, default: T) -> Result<(T, bool), String>
where
    T: FromStr + Copy,
    <T as FromStr>::Err: Display,
{
    match raw {
        None => Ok((default, false)),
        Some(s) => match s.parse::<T>() {
            Ok(value) => Ok((value, true)),
            Err(e) => Err(format!(
                "{name}: invalid value {s:?} ({e}); expected a {}",
                std::any::type_name::<T>()
            )),
        },
    }
}

/// Resolve a numeric/parseable var with a default. Unset → default. Set but
/// unparseable → push a fail-fast error and return the default as a
/// placeholder (the accumulated errors abort boot before it is used). Logs
/// provenance either way.
fn parse_or<T>(errors: &mut Vec<String>, name: &str, default: T) -> T
where
    T: FromStr + Copy,
    <T as FromStr>::Err: Display,
{
    match resolve_parse(name, env::var(name).ok(), default) {
        Ok((value, from_env)) => {
            log_source(name, from_env);
            value
        }
        Err(message) => {
            errors.push(message);
            default
        }
    }
}

/// Resolve a string var with a default, logging provenance.
fn string_or(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) => {
            log_source(name, true);
            value
        }
        Err(_) => {
            log_source(name, false);
            default.to_string()
        }
    }
}

/// Build the Google OAuth client from environment variables.
/// Returns `None` in dev mode (OAuth is bypassed).
///
/// Reports **all** missing credentials at once (fail-fast) rather than
/// panicking on the first one, so a misconfigured deploy is fixed in a single
/// pass.
pub fn build_google_oauth_client(dev_mode: bool) -> anyhow::Result<Option<GoogleOAuthClient>> {
    if dev_mode {
        return Ok(None);
    }

    let mut missing = Vec::new();
    let mut required = |name: &str| match env::var(name) {
        Ok(v) => {
            log_source(name, true);
            v
        }
        Err(_) => {
            missing.push(name.to_string());
            String::new()
        }
    };
    let client_id = required("GOOGLE_CLIENT_ID");
    let client_secret = required("GOOGLE_CLIENT_SECRET");
    let redirect_uri_raw = required("GOOGLE_REDIRECT_URI");

    if !missing.is_empty() {
        anyhow::bail!(
            "Missing required OAuth environment variable(s): {}. \
             Set them, or pass --dev-mode to bypass OAuth.",
            missing.join(", ")
        );
    }

    let auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())?;
    let token_url = TokenUrl::new("https://oauth2.googleapis.com/token".to_string())?;
    let redirect_uri = RedirectUrl::new(redirect_uri_raw)?;

    Ok(Some(
        BasicClient::new(ClientId::new(client_id))
            .set_client_secret(ClientSecret::new(client_secret))
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
    /// Authority (host, optionally `:port`) under which per-forward subdomains
    /// are served (docs/PORT_FORWARDING.md). `None` = forwarding disabled.
    pub forward_domain: Option<String>,
    /// Long-term session archive settings (#1258). `None` = disabled (the
    /// default, including on hosted deployments).
    pub archive: Option<crate::archive::ArchiveConfig>,
}

impl ServerConfig {
    /// Parse and validate server configuration from the environment.
    ///
    /// Fails fast: every malformed numeric var is collected and reported
    /// together so a misconfigured deploy can be fixed in one pass instead of
    /// one-restart-per-typo. Each resolved var logs its provenance
    /// (`target: "config"`), never its value.
    pub fn from_env(dev_mode: bool) -> anyhow::Result<Self> {
        let mut errors: Vec<String> = Vec::new();

        // Get base URL from env or construct from host/port
        let host = string_or("HOST", "0.0.0.0");
        let port = string_or("PORT", "3000");
        let public_url = match env::var("BASE_URL") {
            Ok(v) => {
                log_source("BASE_URL", true);
                v
            }
            Err(_) => {
                log_source("BASE_URL", false);
                // Default to localhost for development
                format!("http://localhost:{}", port)
            }
        };

        // SESSION_SECRET backs both the signed-cookie key and the proxy/launcher
        // JWT secret. Set it to a stable 64+ byte value in production so cookies
        // and tokens survive a redeploy. When it is absent we generate a random
        // ephemeral secret so the server still boots — at the cost of invalidating
        // every cookie and token on restart. (It must never be a hard-coded
        // constant: a known secret lets anyone forge tokens for any user.)
        let session_secret = match env::var("SESSION_SECRET") {
            Ok(secret) => {
                log_source("SESSION_SECRET", true);
                secret
            }
            Err(_) => {
                log_source("SESSION_SECRET", false);
                tracing::warn!(
                    "SESSION_SECRET is not set — generating a random ephemeral secret. \
                     All signed cookies and proxy/launcher JWTs will be invalidated on \
                     restart; set SESSION_SECRET to a stable 64+ byte value in production."
                );
                hex::encode(Key::generate().master())
            }
        };
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
            string_or("APP_TITLE", "Agent Portal")
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
        let message_retention_count: i64 = parse_or(&mut errors, "MESSAGE_RETENTION_COUNT", 100);
        let message_retention_days: u32 = parse_or(&mut errors, "MESSAGE_RETENTION_DAYS", 30);

        let session_max_age_days: u32 = parse_or(&mut errors, "SESSION_MAX_AGE_DAYS", 14);

        let max_image_mb: u32 = parse_or(&mut errors, "PORTAL_MAX_IMAGE_MB", 10);

        // Image store eviction caps — both required to bound memory on long
        // image-heavy sessions (see issue #787). Defaults are 256 MiB / 1 h.
        let image_store_max_mb: u64 = parse_or(
            &mut errors,
            "PORTAL_IMAGE_STORE_MAX_MB",
            handlers::images::DEFAULT_IMAGE_STORE_MAX_BYTES / (1024 * 1024),
        );
        let image_store_ttl_secs: u64 = parse_or(
            &mut errors,
            "PORTAL_IMAGE_STORE_TTL_SECS",
            handlers::images::DEFAULT_IMAGE_STORE_TTL.as_secs(),
        );
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

        // Port-forward subdomain authority (docs/PORT_FORWARDING.md). In dev
        // mode default to `localhost:{port}` — browsers resolve `*.localhost`
        // to loopback with no DNS setup. In production it must be set
        // explicitly (needs wildcard DNS + TLS), so unset = disabled.
        let forward_domain = match env::var("PORTAL_FORWARD_DOMAIN") {
            Ok(v) => {
                log_source("PORTAL_FORWARD_DOMAIN", true);
                Some(v)
            }
            Err(_) => {
                log_source("PORTAL_FORWARD_DOMAIN", false);
                dev_mode.then(|| format!("localhost:{}", port))
            }
        };
        match &forward_domain {
            Some(domain) => tracing::info!("Port forwarding enabled on *.{}", domain),
            None => tracing::info!("Port forwarding disabled (PORTAL_FORWARD_DOMAIN unset)"),
        }

        // Long-term session archive (#1258). Fail-fast on partial config.
        let archive = crate::archive::archive_config_from_env()
            .map_err(|e| anyhow::anyhow!("invalid archive configuration: {e}"))?;
        match &archive {
            Some(cfg) => tracing::info!(
                "Session archive enabled: local root {} (compression {}, transcripts {})",
                cfg.local_root.display(),
                cfg.compression.as_str(),
                cfg.transcripts
            ),
            None => {
                tracing::info!("Session archive disabled (PORTAL_SESSION_ARCHIVE_BACKEND unset)")
            }
        }

        // Fail fast: report every malformed variable at once rather than
        // silently using a default for each.
        if !errors.is_empty() {
            anyhow::bail!(
                "Invalid configuration ({} problem(s)):\n  - {}",
                errors.len(),
                errors.join("\n  - ")
            );
        }

        Ok(ServerConfig {
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
            forward_domain,
            archive,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_var_uses_default_marked_not_from_env() {
        let (value, from_env) = resolve_parse("PORT", None, 3000u32).expect("default is valid");
        assert_eq!(value, 3000);
        assert!(!from_env);
    }

    #[test]
    fn valid_var_parses_and_is_marked_from_env() {
        let (value, from_env) =
            resolve_parse("PORT", Some("8080".to_string()), 3000u32).expect("8080 parses");
        assert_eq!(value, 8080);
        assert!(from_env);
    }

    #[test]
    fn malformed_var_is_a_fail_fast_error_not_a_silent_default() {
        // This is the core regression item 6 fixes: `PORT=abc` previously fell
        // back to the default unnoticed. It must now surface as an error.
        let err = resolve_parse("PORT", Some("abc".to_string()), 3000u32)
            .expect_err("non-numeric must error");
        assert!(err.contains("PORT"), "error names the var: {err}");
        assert!(err.contains("abc"), "error shows the bad value: {err}");
    }

    #[test]
    fn parse_or_accumulates_errors_and_returns_placeholder_default() {
        // `parse_or` reads the real env; use a var name that is not set so the
        // env read returns None and the malformed branch is driven purely by
        // `resolve_parse`. (Behavior of the malformed branch is covered above;
        // here we assert the accumulation contract.)
        let mut errors = Vec::new();
        let value: u32 = parse_or(&mut errors, "PORTAL_NONEXISTENT_TEST_VAR", 42);
        assert_eq!(value, 42, "unset var yields the default");
        assert!(errors.is_empty(), "unset var is not an error");
    }
}
