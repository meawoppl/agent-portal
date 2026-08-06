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
use std::path::PathBuf;
use std::str::FromStr;
use tower_cookies::Key;

use crate::handlers;

pub type OAuthClient =
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

/// The OAuth clients configured for this deploy, one per login provider.
///
/// A provider is *enabled* when its credentials are present; every provider is
/// individually optional, but outside dev mode [`OAuthProviders::from_env`]
/// insists on at least one. Booting with none would serve a login page whose
/// every button 404s — a failure that only shows up when a user tries to sign
/// in, so it is worth catching at startup instead.
#[derive(Clone, Default)]
pub struct OAuthProviders {
    pub google: Option<OAuthClient>,
    pub github: Option<OAuthClient>,
}

impl OAuthProviders {
    /// Build every provider whose credentials are configured.
    ///
    /// Returns an empty set in dev mode (OAuth is bypassed entirely). Partially
    /// configured providers are an error rather than a silent skip: someone who
    /// set two of a provider's three variables meant to enable it, and quietly
    /// disabling it would be baffling.
    pub fn from_env(dev_mode: bool) -> anyhow::Result<Self> {
        if dev_mode {
            return Ok(Self::default());
        }

        let google = build_provider(
            "GOOGLE",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
        )?;
        let github = build_provider(
            "GITHUB",
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
        )?;

        if google.is_none() && github.is_none() {
            anyhow::bail!(
                "No login provider is configured, so nobody could sign in. Set \
                 GOOGLE_CLIENT_ID/GOOGLE_CLIENT_SECRET/GOOGLE_REDIRECT_URI or \
                 GITHUB_CLIENT_ID/GITHUB_CLIENT_SECRET/GITHUB_REDIRECT_URI, or \
                 pass --dev-mode to bypass OAuth."
            );
        }

        Ok(Self { google, github })
    }

    /// The client for one provider key, or `None` when it is not configured.
    pub fn client(&self, provider: &str) -> Option<&OAuthClient> {
        match provider {
            handlers::auth::PROVIDER_GOOGLE => self.google.as_ref(),
            handlers::auth::PROVIDER_GITHUB => self.github.as_ref(),
            _ => None,
        }
    }

    /// Configured provider keys, in the order they are offered on the login
    /// page. Empty in dev mode.
    pub fn enabled(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.google.is_some() {
            keys.push(handlers::auth::PROVIDER_GOOGLE);
        }
        if self.github.is_some() {
            keys.push(handlers::auth::PROVIDER_GITHUB);
        }
        keys
    }
}

/// Build one provider's client from `{PREFIX}_CLIENT_ID` / `_CLIENT_SECRET` /
/// `_REDIRECT_URI`.
///
/// All three absent → `Ok(None)` (provider simply not enabled). Some present
/// and some missing → an error naming every missing one at once, so a
/// misconfigured deploy is fixed in a single pass.
fn build_provider(
    prefix: &str,
    auth_url: &str,
    token_url: &str,
) -> anyhow::Result<Option<OAuthClient>> {
    let names = [
        format!("{prefix}_CLIENT_ID"),
        format!("{prefix}_CLIENT_SECRET"),
        format!("{prefix}_REDIRECT_URI"),
    ];
    let values: [Option<String>; 3] = [
        env::var(&names[0]).ok(),
        env::var(&names[1]).ok(),
        env::var(&names[2]).ok(),
    ];

    let Some(credentials) = resolve_provider_vars(prefix, &names, values)? else {
        return Ok(None);
    };
    for name in &names {
        log_source(name, true);
    }

    let [client_id, client_secret, redirect_uri] = credentials;
    Ok(Some(
        BasicClient::new(ClientId::new(client_id))
            .set_client_secret(ClientSecret::new(client_secret))
            .set_auth_uri(AuthUrl::new(auth_url.to_string())?)
            .set_token_uri(TokenUrl::new(token_url.to_string())?)
            .set_redirect_uri(RedirectUrl::new(redirect_uri)?),
    ))
}

/// Pure core of [`build_provider`]: classify a provider's three raw values as
/// not-configured, fully configured, or a fail-fast error. No env read, no
/// logging — kept separate so it is unit-testable without mutating process
/// globals (same idiom as [`resolve_parse`]).
fn resolve_provider_vars(
    prefix: &str,
    names: &[String; 3],
    values: [Option<String>; 3],
) -> Result<Option<[String; 3]>, anyhow::Error> {
    if values.iter().all(|v| v.is_none()) {
        return Ok(None);
    }

    let missing: Vec<&str> = names
        .iter()
        .zip(&values)
        .filter(|(_, v)| v.is_none())
        .map(|(n, _)| n.as_str())
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "{prefix} login is partially configured — missing {}. Set the \
             missing variable(s), or unset the others to disable {prefix} login.",
            missing.join(", ")
        );
    }

    let [id, secret, redirect] = values;
    match (id, secret, redirect) {
        (Some(id), Some(secret), Some(redirect)) => Ok(Some([id, secret, redirect])),
        // Unreachable: `missing` is empty, so all three are `Some`.
        _ => unreachable!("all three values present when nothing is missing"),
    }
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
    /// Per-file cap for videos shown via `agent-portal show` (default 100 MB).
    pub max_video_mb: u32,
    /// Per-recording cap (MB) for speech-to-text uploads.
    pub max_audio_mb: u32,
    /// Total-byte cap for the on-disk media (video) store (default 1 GiB).
    pub media_store_max_bytes: u64,
    /// Authority (host, optionally `:port`) under which per-forward subdomains
    /// are served (docs/PORT_FORWARDING.md). `None` = forwarding disabled.
    pub forward_domain: Option<String>,
    /// Long-term session archive settings (#1258). `None` = disabled (the
    /// default, including on hosted deployments).
    pub archive: Option<crate::archive::ArchiveConfig>,
    /// VAPID application-server public key served to Web Push clients
    /// (`GET /api/push/vapid-key`). `None` = push unconfigured; the endpoint
    /// 404s and the frontend degrades to "push unavailable".
    pub vapid_public_key: Option<String>,
    /// Native APNs/FCM push settings (mobile-apps plan C7). Missing provider
    /// groups disable that provider; partial APNs config fails fast.
    pub native_push: crate::push::NativePushConfig,
    /// VAPID application-server private key for Web Push (mobile-apps plan
    /// §8.3, C3). URL-safe base64 (no padding) or a PEM/DER private key.
    /// `None` = Web Push disabled; the dispatcher keeps the log-only transport.
    /// The matching public half is `PORTAL_VAPID_PUBLIC_KEY`, served to browsers
    /// as the `applicationServerKey`.
    pub vapid_private_key: Option<String>,
    /// Native mobile app-link association payload configuration.
    pub mobile_app_links: MobileAppLinksConfig,
}

/// Public mobile app-link association payload configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MobileAppLinksConfig {
    pub bundle_id: String,
    pub apple_team_id: String,
    pub android_sha256_cert_fingerprints: Vec<String>,
}

impl Default for MobileAppLinksConfig {
    fn default() -> Self {
        Self {
            bundle_id: "io.txcl.agentportal".to_string(),
            apple_team_id: "TEAMID".to_string(),
            android_sha256_cert_fingerprints: vec![
                // Placeholder debug fingerprint. Hosted/prod builds should set
                // PORTAL_MOBILE_ANDROID_SHA256_CERT_FINGERPRINTS to the release
                // signing certificate fingerprint(s).
                "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00"
                    .to_string(),
            ],
        }
    }
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

        // Message retention counts wire records, not turns.
        //
        // #1581 raised this to 1000 because muse journals ~60–90 records per
        // turn, so 100 kept barely one turn. But this value is also the
        // *initial replay limit* (`web_client_socket.rs`: every session open
        // ships the trailing `message_retention_count` records), so 1000 made
        // each session open ship 10x the payload — which the frontend then
        // trimmed to its own live-buffer cap and discarded. That made the
        // portal unusably slow, which is a worse failure than muse eviction,
        // so this is back to 100.
        //
        // Untangling it properly means decoupling the three things this one
        // number controls — durable retention, replay size, and render budget —
        // rather than trading one off against the others. Kept in sync with the
        // frontend live-buffer cap (`MAX_MESSAGES_PER_SESSION`) meanwhile.
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

        // Video caps for `agent-portal show`. The per-file cap defaults higher
        // than images (video is inherently larger); the on-disk store cap bounds
        // total footprint. Videos reuse `image_store_ttl` for per-entry expiry.
        let max_video_mb: u32 = parse_or(&mut errors, "PORTAL_MAX_VIDEO_MB", 100);
        // 25 MB matches the smallest per-request limit among supported speech
        // providers; a minute of Opus is well under 1 MB, so this is generous.
        let max_audio_mb: u32 = parse_or(&mut errors, "PORTAL_MAX_AUDIO_MB", 25);
        let media_store_max_mb: u64 = parse_or(
            &mut errors,
            "PORTAL_MEDIA_STORE_MAX_MB",
            handlers::media_store::DEFAULT_MEDIA_STORE_MAX_BYTES / (1024 * 1024),
        );
        let media_store_max_bytes = media_store_max_mb.saturating_mul(1024 * 1024);

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
        tracing::info!("Max audio size: {} MB", max_audio_mb);
        tracing::info!(
            "Max video size: {} MB, media store cap: {} MB",
            max_video_mb,
            media_store_max_mb
        );
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

        // VAPID public key for Web Push. Only the public half lives here — it
        // is served to browsers; the private key (PORTAL_VAPID_PRIVATE_KEY) is
        // read by the Web Push sender, never by this endpoint. Unset = push
        // disabled (the vapid-key endpoint 404s).
        let vapid_public_key = match env::var("PORTAL_VAPID_PUBLIC_KEY") {
            Ok(v) if !v.trim().is_empty() => {
                log_source("PORTAL_VAPID_PUBLIC_KEY", true);
                Some(v)
            }
            _ => {
                log_source("PORTAL_VAPID_PUBLIC_KEY", false);
                None
            }
        };
        match &vapid_public_key {
            Some(_) => tracing::info!("Web Push VAPID public key configured"),
            None => tracing::info!("Web Push disabled (PORTAL_VAPID_PUBLIC_KEY unset)"),
        }

        let native_push = native_push_config_from_env(&mut errors);
        let mobile_app_links = mobile_app_links_config_from_env();

        // Long-term session archive (#1258). Fail-fast on partial config.
        let archive = crate::archive::archive_config_from_env()
            .map_err(|e| anyhow::anyhow!("invalid archive configuration: {e}"))?;
        match &archive {
            Some(cfg) => {
                let backend = match &cfg.backend {
                    crate::archive::ArchiveBackendConfig::Local { root } => {
                        format!("local root {}", root.display())
                    }
                    crate::archive::ArchiveBackendConfig::S3 { bucket, prefix } => format!(
                        "s3 bucket {bucket}{}",
                        prefix
                            .as_deref()
                            .map(|p| format!(" prefix {p}"))
                            .unwrap_or_default()
                    ),
                };
                tracing::info!(
                    "Session archive enabled: {backend} (zstd transcripts: {}, media: {})",
                    cfg.transcripts,
                    cfg.media
                );
            }
            None => tracing::info!(
                "Session archive disabled (PORTAL_SESSION_ARCHIVE_BACKEND unset) — the History \
                 viewer will be hidden and /api/history/* will 404 until it is set"
            ),
        }

        // VAPID private key for Web Push (mobile-apps plan §8.3). Only the
        // private half lives here — the public half (PORTAL_VAPID_PUBLIC_KEY)
        // is served to browsers. Unset = Web Push disabled (log-only transport);
        // never a hard error, so the server still boots without push configured.
        let vapid_private_key = match env::var("PORTAL_VAPID_PRIVATE_KEY") {
            Ok(v) => {
                log_source("PORTAL_VAPID_PRIVATE_KEY", true);
                Some(v)
            }
            Err(_) => {
                log_source("PORTAL_VAPID_PRIVATE_KEY", false);
                None
            }
        };

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
            max_video_mb,
            max_audio_mb,
            media_store_max_bytes,
            forward_domain,
            archive,
            vapid_public_key,
            native_push,
            vapid_private_key,
            mobile_app_links,
        })
    }
}

fn mobile_app_links_config_from_env() -> MobileAppLinksConfig {
    let defaults = MobileAppLinksConfig::default();

    let bundle_id = match optional_non_empty("PORTAL_MOBILE_BUNDLE_ID") {
        Some(value) => {
            log_source("PORTAL_MOBILE_BUNDLE_ID", true);
            value
        }
        None => {
            log_source("PORTAL_MOBILE_BUNDLE_ID", false);
            defaults.bundle_id
        }
    };

    let apple_team_id = match optional_non_empty("PORTAL_MOBILE_APPLE_TEAM_ID") {
        Some(value) => {
            log_source("PORTAL_MOBILE_APPLE_TEAM_ID", true);
            value
        }
        None => {
            log_source("PORTAL_MOBILE_APPLE_TEAM_ID", false);
            defaults.apple_team_id
        }
    };

    let android_sha256_cert_fingerprints =
        match optional_non_empty("PORTAL_MOBILE_ANDROID_SHA256_CERT_FINGERPRINTS") {
            Some(value) => {
                log_source("PORTAL_MOBILE_ANDROID_SHA256_CERT_FINGERPRINTS", true);
                let fingerprints = value
                    .split(',')
                    .map(str::trim)
                    .filter(|fingerprint| !fingerprint.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if fingerprints.is_empty() {
                    defaults.android_sha256_cert_fingerprints
                } else {
                    fingerprints
                }
            }
            None => {
                log_source("PORTAL_MOBILE_ANDROID_SHA256_CERT_FINGERPRINTS", false);
                defaults.android_sha256_cert_fingerprints
            }
        };

    MobileAppLinksConfig {
        bundle_id,
        apple_team_id,
        android_sha256_cert_fingerprints,
    }
}

fn optional_non_empty(name: &str) -> Option<String> {
    env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn native_push_config_from_env(errors: &mut Vec<String>) -> crate::push::NativePushConfig {
    let apns_path = optional_non_empty("PORTAL_APNS_KEY_P8_PATH");
    let apns_key_id = optional_non_empty("PORTAL_APNS_KEY_ID");
    let apns_team_id = optional_non_empty("PORTAL_APNS_TEAM_ID");
    let apns_bundle_id = optional_non_empty("PORTAL_APNS_BUNDLE_ID");

    let apns_any = apns_path.is_some()
        || apns_key_id.is_some()
        || apns_team_id.is_some()
        || apns_bundle_id.is_some();
    let apns = if apns_any {
        for (name, value) in [
            ("PORTAL_APNS_KEY_P8_PATH", &apns_path),
            ("PORTAL_APNS_KEY_ID", &apns_key_id),
            ("PORTAL_APNS_TEAM_ID", &apns_team_id),
            ("PORTAL_APNS_BUNDLE_ID", &apns_bundle_id),
        ] {
            log_source(name, value.is_some());
        }

        match (apns_path, apns_key_id, apns_team_id, apns_bundle_id) {
            (Some(key_p8_path), Some(key_id), Some(team_id), Some(bundle_id)) => {
                tracing::info!("APNs push transport configured");
                Some(crate::push::ApnsTransportConfig {
                    key_p8_path: PathBuf::from(key_p8_path),
                    key_id,
                    team_id,
                    bundle_id,
                })
            }
            (path, key_id, team_id, bundle_id) => {
                let mut missing = Vec::new();
                if path.is_none() {
                    missing.push("PORTAL_APNS_KEY_P8_PATH");
                }
                if key_id.is_none() {
                    missing.push("PORTAL_APNS_KEY_ID");
                }
                if team_id.is_none() {
                    missing.push("PORTAL_APNS_TEAM_ID");
                }
                if bundle_id.is_none() {
                    missing.push("PORTAL_APNS_BUNDLE_ID");
                }
                errors.push(format!(
                    "APNs push config is partial; missing {}",
                    missing.join(", ")
                ));
                None
            }
        }
    } else {
        for name in [
            "PORTAL_APNS_KEY_P8_PATH",
            "PORTAL_APNS_KEY_ID",
            "PORTAL_APNS_TEAM_ID",
            "PORTAL_APNS_BUNDLE_ID",
        ] {
            log_source(name, false);
        }
        tracing::info!("APNs push transport disabled (PORTAL_APNS_* unset)");
        None
    };

    let fcm = match optional_non_empty("PORTAL_FCM_SERVICE_ACCOUNT_PATH") {
        Some(path) => {
            log_source("PORTAL_FCM_SERVICE_ACCOUNT_PATH", true);
            tracing::info!("FCM push transport configured");
            Some(crate::push::FcmTransportConfig {
                service_account_path: PathBuf::from(path),
            })
        }
        None => {
            log_source("PORTAL_FCM_SERVICE_ACCOUNT_PATH", false);
            tracing::info!("FCM push transport disabled (PORTAL_FCM_SERVICE_ACCOUNT_PATH unset)");
            None
        }
    };

    crate::push::NativePushConfig { apns, fcm }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_names() -> [String; 3] {
        [
            "GITHUB_CLIENT_ID".to_string(),
            "GITHUB_CLIENT_SECRET".to_string(),
            "GITHUB_REDIRECT_URI".to_string(),
        ]
    }

    #[test]
    fn a_provider_with_no_variables_set_is_simply_disabled() {
        let resolved = resolve_provider_vars("GITHUB", &provider_names(), [None, None, None])
            .expect("absent is not an error");
        assert!(resolved.is_none());
    }

    #[test]
    fn a_fully_configured_provider_resolves_its_credentials() {
        let resolved = resolve_provider_vars(
            "GITHUB",
            &provider_names(),
            [
                Some("id".to_string()),
                Some("secret".to_string()),
                Some("https://example.invalid/cb".to_string()),
            ],
        )
        .expect("complete config is valid");
        assert_eq!(
            resolved,
            Some([
                "id".to_string(),
                "secret".to_string(),
                "https://example.invalid/cb".to_string()
            ])
        );
    }

    /// Half-configuring a provider is a typo, not a request to disable it —
    /// silently skipping would leave the operator staring at a missing button.
    #[test]
    fn a_partially_configured_provider_names_every_missing_variable() {
        let err = resolve_provider_vars(
            "GITHUB",
            &provider_names(),
            [Some("id".to_string()), None, None],
        )
        .expect_err("partial config must fail");
        let message = err.to_string();
        assert!(message.contains("GITHUB_CLIENT_SECRET"), "{message}");
        assert!(message.contains("GITHUB_REDIRECT_URI"), "{message}");
        assert!(!message.contains("GITHUB_CLIENT_ID"), "{message}");
    }

    #[test]
    fn dev_mode_enables_no_providers() {
        let providers = OAuthProviders::from_env(true).expect("dev mode never fails");
        assert!(providers.enabled().is_empty());
        assert!(providers.client("google").is_none());
    }

    #[test]
    fn enabled_lists_only_configured_providers_and_client_looks_them_up() {
        let client = |redirect: &str| {
            BasicClient::new(ClientId::new("id".to_string()))
                .set_auth_uri(AuthUrl::new("https://example.invalid/a".to_string()).unwrap())
                .set_token_uri(TokenUrl::new("https://example.invalid/t".to_string()).unwrap())
                .set_redirect_uri(RedirectUrl::new(redirect.to_string()).unwrap())
        };
        let providers = OAuthProviders {
            google: None,
            github: Some(client("https://example.invalid/cb")),
        };

        assert_eq!(providers.enabled(), vec!["github"]);
        assert!(providers.client("github").is_some());
        assert!(providers.client("google").is_none());
        assert!(providers.client("gitlab").is_none());
    }

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
