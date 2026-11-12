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
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Path, PathBuf};
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

/// Names the file holding the session secret, as an alternative to putting it
/// in `SESSION_SECRET` directly.
const SESSION_SECRET_FILE_VAR: &str = "SESSION_SECRET_FILE";

/// Upper bound on the session secret file. A secret is tens of bytes; anything
/// this large is the wrong file.
const SESSION_SECRET_FILE_MAX_BYTES: u64 = 4096;

/// Where the session secret came from. Logged at boot; the value never is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionSecretSource {
    Env,
    File,
    Ephemeral,
}

impl SessionSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Ephemeral => "ephemeral",
        }
    }
}

/// Resolve the session secret, which backs both the signed-cookie key and the
/// proxy/launcher JWT secret. Rotating it logs out every browser and forces
/// every launcher to re-authenticate.
///
/// - `SESSION_SECRET_FILE` set: the file's contents, minus one trailing
///   `\n` or `\r\n`, are used exactly as `SESSION_SECRET` would be. A missing,
///   unreadable, unsafe, empty or non-UTF-8 file is an error, never a fallback.
/// - Both set: an error, so it is never ambiguous which one is live.
/// - Only `SESSION_SECRET` set: its value, unchanged.
/// - Neither set: a random ephemeral secret, so a dev server still boots at the
///   cost of invalidating every cookie and token on restart. It must never be a
///   hard-coded constant: a known secret lets anyone forge tokens for any user.
///
/// Error messages never contain the secret or its length.
fn resolve_session_secret(
    env_value: Option<OsString>,
    file_path: Option<OsString>,
) -> Result<(String, SessionSecretSource), String> {
    match (env_value, file_path) {
        (Some(_), Some(_)) => Err(format!(
            "SESSION_SECRET and {SESSION_SECRET_FILE_VAR} are both set; set exactly one"
        )),
        (None, Some(path)) if path.is_empty() => {
            Err(format!("{SESSION_SECRET_FILE_VAR} is set but empty"))
        }
        (None, Some(path)) => {
            read_session_secret_file(Path::new(&path)).map(|s| (s, SessionSecretSource::File))
        }
        // A non-UTF-8 SESSION_SECRET is treated as unset.
        (Some(value), None) => match value.into_string() {
            Ok(secret) => Ok((secret, SessionSecretSource::Env)),
            Err(_) => Ok(ephemeral_session_secret()),
        },
        (None, None) => Ok(ephemeral_session_secret()),
    }
}

fn ephemeral_session_secret() -> (String, SessionSecretSource) {
    (
        hex::encode(Key::generate().master()),
        SessionSecretSource::Ephemeral,
    )
}

/// Read the session secret from an injected file (typically a read-only bind
/// mount). Validates the opened descriptor rather than the path, so the checks
/// apply to exactly what is read. The file must be a regular file owned by root
/// or the backend user and not group- or world-writable: root-owned 0444 and
/// backend-user-owned 0400/0600 are accepted.
fn read_session_secret_file(path: &Path) -> Result<String, String> {
    let name = SESSION_SECRET_FILE_VAR;
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(format!("{name} requires Unix permission checks"))
    }
    #[cfg(unix)]
    {
        use std::{
            io::Read,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };
        use zeroize::Zeroizing;
        let shown = path.display();
        // O_NONBLOCK so a FIFO at the path fails the regular-file check below
        // instead of blocking boot.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| format!("{name}: cannot open {shown}: {e}"))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("{name}: cannot stat {shown}: {e}"))?;
        if !metadata.is_file() {
            return Err(format!("{name}: {shown} is not a regular file"));
        }
        if metadata.mode() & 0o022 != 0 {
            return Err(format!(
                "{name}: {shown} is group- or world-writable (mode {:04o}); use 0400, 0600 or 0444",
                metadata.mode() & 0o7777
            ));
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != 0 && metadata.uid() != euid {
            return Err(format!(
                "{name}: {shown} is owned by uid {}; it must be owned by root or the backend user (uid {euid})",
                metadata.uid()
            ));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(SESSION_SECRET_FILE_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{name}: cannot read {shown}: {e}"))?;
        if bytes.len() as u64 > SESSION_SECRET_FILE_MAX_BYTES {
            return Err(format!(
                "{name}: {shown} exceeds {SESSION_SECRET_FILE_MAX_BYTES} bytes"
            ));
        }
        session_secret_from_file_bytes(&bytes)
            .map_err(|problem| format!("{name}: {shown} {problem}"))
    }
}

/// Strip exactly one trailing `\n` or `\r\n` (the newline `echo` or an editor
/// appends) and require a non-empty UTF-8 value.
fn session_secret_from_file_bytes(bytes: &[u8]) -> Result<String, &'static str> {
    let trimmed = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    if trimmed.is_empty() {
        return Err("is empty");
    }
    std::str::from_utf8(trimmed)
        .map(str::to_owned)
        .map_err(|_| "is not valid UTF-8")
}

/// Derive the signed-cookie key from the session secret: its first 64 bytes,
/// zero-padded if shorter.
fn cookie_key_from_secret(secret: &str) -> Key {
    let bytes = secret.as_bytes();
    if bytes.len() < 64 {
        tracing::warn!("SESSION_SECRET should be at least 64 bytes, padding with zeros");
        let mut padded = vec![0u8; 64];
        padded[..bytes.len()].copy_from_slice(bytes);
        Key::from(&padded)
    } else {
        Key::from(&bytes[..64])
    }
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
    /// Optional administrator override, validated against the private key.
    /// When absent, startup derives the public key from the resolved identity.
    pub vapid_public_key: Option<String>,
    /// Native APNs/FCM push settings (mobile-apps plan C7). Missing provider
    /// groups disable that provider; partial APNs config fails fast.
    pub native_push: crate::push::NativePushConfig,
    /// Optional VAPID private-key override: URL-safe base64 (no padding) or PEM.
    /// When absent, startup loads or generates the persistent database identity.
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

        // The session secret backs both the signed-cookie key and the
        // proxy/launcher JWT secret; see `resolve_session_secret`.
        let session_secret = match resolve_session_secret(
            env::var_os("SESSION_SECRET"),
            env::var_os(SESSION_SECRET_FILE_VAR),
        ) {
            Ok((secret, source)) => {
                tracing::info!(
                    target: "config",
                    var = "SESSION_SECRET",
                    source = source.as_str(),
                );
                if source == SessionSecretSource::Ephemeral {
                    tracing::warn!(
                        "Neither SESSION_SECRET nor SESSION_SECRET_FILE is set — generating a \
                         random ephemeral secret. All signed cookies and proxy/launcher JWTs \
                         will be invalidated on restart; provide a stable 64+ byte secret in \
                         production."
                    );
                }
                Some(secret)
            }
            Err(message) => {
                errors.push(message);
                None
            }
        };

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
                .filter(|e| shared::strings::is_non_empty(e))
                .collect::<Vec<_>>()
        });

        if allowed_email_domain.is_some() || allowed_emails.is_some() {
            tracing::info!(
                "Email access control enabled: domain={:?}, specific_emails={}",
                allowed_email_domain,
                allowed_emails.as_ref().map_or(0, |e| e.len())
            );
        }

        // Durable per-session message budget, counted in wire records.
        //
        // This is deliberately NOT the same knob as the frontend's render budget
        // (`MAX_MESSAGES_PER_SESSION`), even though both were 1000 before. They
        // answer different questions — how much history is kept, versus how much
        // is drawn — and the portal-slowness incident that lowered the render
        // budget was not a reason to delete history. Keeping this high is the
        // conservative side: trimming is irreversible, over-rendering is not.
        //
        // One coupling remains and is worth knowing about: `web_client_socket.rs`
        // sets `initial_replay_limit` from this value, so a cold session open
        // ships the trailing N records and the frontend discards all but its
        // render budget. That is wasted bytes, not the cause of the slowness
        // people reported (measured: the render budget was), so it's an
        // efficiency fix rather than an urgent one — bound the replay separately
        // instead of lowering retention to compensate.
        let message_retention_count: i64 = parse_or(&mut errors, "MESSAGE_RETENTION_COUNT", 1000);
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

        // Optional explicit identity. Startup resolves the database default
        // and validates any configured public/private pair before serving.
        let vapid_public_key = match env::var("PORTAL_VAPID_PUBLIC_KEY") {
            Ok(v) if shared::strings::is_non_blank(&v) => {
                log_source("PORTAL_VAPID_PUBLIC_KEY", true);
                Some(v)
            }
            _ => {
                log_source("PORTAL_VAPID_PUBLIC_KEY", false);
                None
            }
        };

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

        // Private overrides stay in the backend, never in an API response.
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

        let Some(session_secret) = session_secret else {
            anyhow::bail!("session secret unresolved");
        };
        let cookie_key = cookie_key_from_secret(&session_secret);
        // JWT secret for proxy tokens — same source as the cookie key above.
        let jwt_secret = session_secret;

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
                    .filter(|fingerprint| shared::strings::is_non_empty(fingerprint))
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
    env::var(name)
        .ok()
        .and_then(|value| shared::strings::owned_non_blank(&value))
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

    #[cfg(unix)]
    mod session_secret {
        use super::*;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        const SECRET: &str =
            "a-stable-session-secret-that-is-comfortably-over-64-bytes-long-0123456789";

        /// A secret file with the given contents and mode, inside a temp dir
        /// that is dropped (and removed) with the returned guard.
        fn secret_file(contents: &[u8], mode: u32) -> (tempfile::TempDir, OsString) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.key");
            std::fs::File::create(&path)
                .unwrap()
                .write_all(contents)
                .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            (dir, path.into_os_string())
        }

        fn from_file(path: OsString) -> Result<(String, SessionSecretSource), String> {
            resolve_session_secret(None, Some(path))
        }

        #[test]
        fn file_contents_are_used_as_the_secret() {
            let (_dir, path) = secret_file(SECRET.as_bytes(), 0o400);
            let (secret, source) = from_file(path).unwrap();
            assert_eq!(secret, SECRET);
            assert_eq!(source, SessionSecretSource::File);
        }

        #[test]
        fn exactly_one_trailing_newline_is_stripped() {
            for (contents, expected) in [
                (format!("{SECRET}\n"), SECRET.to_string()),
                (format!("{SECRET}\r\n"), SECRET.to_string()),
                (format!("{SECRET}\n\n"), format!("{SECRET}\n")),
                (format!(" {SECRET} "), format!(" {SECRET} ")),
            ] {
                let (_dir, path) = secret_file(contents.as_bytes(), 0o600);
                assert_eq!(from_file(path).unwrap().0, expected, "{contents:?}");
            }
        }

        #[test]
        fn file_and_env_yield_byte_identical_keys() {
            let (_dir, path) = secret_file(format!("{SECRET}\n").as_bytes(), 0o444);
            let (from_file, _) = from_file(path).unwrap();
            let (from_env, source) =
                resolve_session_secret(Some(OsString::from(SECRET)), None).unwrap();
            assert_eq!(source, SessionSecretSource::Env);
            assert_eq!(from_file.as_bytes(), from_env.as_bytes());
            assert_eq!(
                cookie_key_from_secret(&from_file).master(),
                cookie_key_from_secret(&from_env).master()
            );
        }

        #[test]
        fn both_set_is_an_error() {
            let (_dir, path) = secret_file(SECRET.as_bytes(), 0o400);
            let err = resolve_session_secret(Some(OsString::from(SECRET)), Some(path))
                .expect_err("ambiguous configuration must not boot");
            assert!(err.contains("SESSION_SECRET_FILE"), "{err}");
            assert!(!err.contains(SECRET), "error must not leak the secret");
        }

        #[test]
        fn missing_file_is_an_error_not_a_fallback() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("absent.key").into_os_string();
            let err = from_file(path).expect_err("missing file must not boot");
            assert!(err.contains("cannot open"), "{err}");
        }

        #[test]
        fn empty_file_is_an_error() {
            for contents in [&b""[..], b"\n", b"\r\n"] {
                let (_dir, path) = secret_file(contents, 0o400);
                let err = from_file(path).expect_err("empty secret must not boot");
                assert!(err.contains("is empty"), "{contents:?}: {err}");
            }
        }

        #[test]
        fn empty_path_is_an_error() {
            assert!(from_file(OsString::new()).is_err());
        }

        #[test]
        fn non_utf8_file_is_an_error() {
            let (_dir, path) = secret_file(&[0xff, 0xfe, 0x00], 0o400);
            let err = from_file(path).expect_err("non-UTF-8 must not boot");
            assert!(err.contains("UTF-8"), "{err}");
        }

        #[test]
        fn oversized_file_is_an_error() {
            let contents = vec![b'a'; SESSION_SECRET_FILE_MAX_BYTES as usize + 1];
            let (_dir, path) = secret_file(&contents, 0o400);
            assert!(from_file(path).is_err());
        }

        #[test]
        fn group_or_world_writable_file_is_refused() {
            for mode in [0o620, 0o602, 0o666] {
                let (_dir, path) = secret_file(SECRET.as_bytes(), mode);
                let err = from_file(path).expect_err("writable-by-others must not boot");
                assert!(err.contains("writable"), "{mode:o}: {err}");
            }
        }

        #[test]
        fn directory_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let err = from_file(dir.path().as_os_str().to_owned())
                .expect_err("a directory is not a secret file");
            assert!(err.contains("regular file"), "{err}");
        }

        #[test]
        fn env_only_and_neither_are_unchanged() {
            let (secret, source) =
                resolve_session_secret(Some(OsString::from(SECRET)), None).unwrap();
            assert_eq!(
                (secret.as_str(), source),
                (SECRET, SessionSecretSource::Env)
            );

            let (a, source) = resolve_session_secret(None, None).unwrap();
            let (b, _) = resolve_session_secret(None, None).unwrap();
            assert_eq!(source, SessionSecretSource::Ephemeral);
            assert_eq!(a.len(), 128, "hex of a 64-byte key");
            assert_ne!(a, b, "ephemeral secrets are random");
        }
    }
}
