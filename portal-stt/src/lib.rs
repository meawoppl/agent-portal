//! Server-side speech-to-text, behind a closed set of providers.
//!
//! Voice input has historically been browser-native (`SpeechRecognition`), and
//! still is when no provider is configured. That path has a hard ceiling for a
//! coding portal: it is unavailable in Firefox, it is one-utterance-per-tap, and
//! — the part that actually hurts — it has **no vocabulary hook**, so `clippy`,
//! `Diesel`, `Axum`, branch names and file paths come back mangled.
//!
//! A hosted provider fixes all three, and the accuracy win comes mostly from
//! [`keyterms`], which turns what we already know about a session (repo, branch,
//! working directory, agent) into a bias list. That context is the thing the
//! browser API can never have.
//!
//! ## Why an enum and not a trait object
//!
//! Same reasoning as `ArchiveStore`: providers are a closed, exhaustively
//! matched set, so adding one is a compile error until every arm is filled in.
//! It also keeps `async fn` in the impl rather than dragging in `async-trait`.
//!
//! ## Why this does not repeat the Google Cloud STT mistake
//!
//! Server STT was removed in 2.6.x because it meant a GCP service account, a
//! gRPC client, a `/ws/voice/{id}` route and a PCM `AudioWorklet` — roughly 600
//! lines that every self-hoster had to configure. This design keeps the cost
//! proportional: providers are plain HTTP over the existing `reqwest` client,
//! the transport is one request with the recorded blob, and **the whole feature
//! is off unless `PORTAL_STT_BACKEND` is set**, in which case the browser path
//! stays exactly as it is today.

pub use bytes::Bytes;

mod keyterms;
mod providers;

pub use keyterms::session_keyterms;

/// Environment variable selecting the provider (`disabled`, `openai`,
/// `deepgram`). Named to match `PORTAL_SESSION_ARCHIVE_BACKEND`.
const BACKEND_VAR: &str = "PORTAL_STT_BACKEND";
const API_KEY_VAR: &str = "PORTAL_STT_API_KEY";
const MODEL_VAR: &str = "PORTAL_STT_MODEL";

/// What the caller wants transcribed, plus the context to bias it with.
pub struct TranscribeRequest<'a> {
    pub audio: Bytes,
    /// The recording's MIME type, e.g. `audio/webm`. Providers need it to pick
    /// a decoder (Deepgram) or a filename extension (OpenAI's multipart form).
    pub content_type: &'a str,
    /// BCP-47 tag, when the client knows it. `None` lets the provider decide.
    pub language: Option<&'a str>,
    /// Vocabulary hints — see [`session_keyterms`]. May be empty.
    pub keyterms: &'a [String],
}

#[derive(Debug)]
pub enum SttError {
    /// The provider was reachable but rejected the request (bad key, bad audio,
    /// quota). Carries the provider's own message where we have one.
    Provider(String),
    /// Network/transport failure talking to the provider.
    Transport(String),
    /// The provider answered with a shape we could not read.
    Decode(String),
}

impl std::fmt::Display for SttError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(m) => write!(f, "provider rejected the request: {m}"),
            Self::Transport(m) => write!(f, "could not reach the speech provider: {m}"),
            Self::Decode(m) => write!(f, "unreadable response from the speech provider: {m}"),
        }
    }
}

/// A configured speech-to-text provider.
#[derive(Clone)]
pub enum SttProvider {
    OpenAi(providers::openai::OpenAiStt),
    Deepgram(providers::deepgram::DeepgramStt),
}

impl SttProvider {
    /// Build the configured provider, or `None` when STT is disabled.
    ///
    /// Fails fast on a selected-but-unconfigured provider rather than silently
    /// falling back to the browser path — a deploy that meant to enable STT
    /// should say so loudly at boot, not degrade quietly at first use.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let backend = std::env::var(BACKEND_VAR).unwrap_or_else(|_| "disabled".to_string());
        let backend = backend.trim().to_ascii_lowercase();
        if backend.is_empty() || backend == "disabled" {
            return Ok(None);
        }

        let api_key = std::env::var(API_KEY_VAR).map_err(|_| {
            anyhow::anyhow!("{BACKEND_VAR}={backend} requires {API_KEY_VAR} to be set")
        })?;
        if api_key.trim().is_empty() {
            anyhow::bail!("{API_KEY_VAR} is set but empty");
        }
        let model = std::env::var(MODEL_VAR)
            .ok()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());

        match backend.as_str() {
            "openai" => Ok(Some(Self::OpenAi(providers::openai::OpenAiStt::new(
                api_key, model,
            )))),
            "deepgram" => Ok(Some(Self::Deepgram(providers::deepgram::DeepgramStt::new(
                api_key, model,
            )))),
            other => anyhow::bail!(
                "unknown {BACKEND_VAR}={other:?}; expected one of: disabled, openai, deepgram"
            ),
        }
    }

    /// Stable key for logs and `/api/config`.
    pub fn key(&self) -> &'static str {
        match self {
            Self::OpenAi(_) => "openai",
            Self::Deepgram(_) => "deepgram",
        }
    }

    pub async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<String, SttError> {
        match self {
            Self::OpenAi(provider) => provider.transcribe(request).await,
            Self::Deepgram(provider) => provider.transcribe(request).await,
        }
    }
}

/// Map a recording's MIME type to the filename extension providers expect when
/// the audio is uploaded as a file. Unknown types fall back to `webm`, which is
/// what every browser `MediaRecorder` we target produces.
pub(crate) fn extension_for(content_type: &str) -> &'static str {
    match content_type.split(';').next().unwrap_or("").trim() {
        "audio/mp4" | "audio/x-m4a" => "mp4",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        _ => "webm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_env` reads process globals, so these run serially under one lock
    /// and restore what they touched — matching the archive-config tests.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard;

    impl EnvGuard {
        fn set(vars: &[(&str, Option<&str>)]) -> Self {
            for (name, value) in vars {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
            Self
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for name in [BACKEND_VAR, API_KEY_VAR, MODEL_VAR] {
                std::env::remove_var(name);
            }
        }
    }

    #[test]
    fn stt_is_off_unless_a_backend_is_named() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[(BACKEND_VAR, None), (API_KEY_VAR, None)]);
        assert!(SttProvider::from_env()
            .expect("absent is not an error")
            .is_none());

        let _guard = EnvGuard::set(&[(BACKEND_VAR, Some("disabled"))]);
        assert!(SttProvider::from_env()
            .expect("disabled is explicit")
            .is_none());
    }

    #[test]
    fn each_backend_builds_its_own_provider() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let _guard = EnvGuard::set(&[(BACKEND_VAR, Some("openai")), (API_KEY_VAR, Some("k"))]);
        assert_eq!(
            SttProvider::from_env()
                .expect("configured")
                .expect("some")
                .key(),
            "openai"
        );

        let _guard = EnvGuard::set(&[(BACKEND_VAR, Some("Deepgram")), (API_KEY_VAR, Some("k"))]);
        assert_eq!(
            SttProvider::from_env()
                .expect("case-insensitive")
                .expect("some")
                .key(),
            "deepgram"
        );
    }

    /// Selecting a backend and forgetting the key is a misconfiguration the
    /// operator wants to hear about at boot, not a reason to quietly serve the
    /// browser path.
    #[test]
    fn a_backend_without_a_key_fails_boot() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[(BACKEND_VAR, Some("openai")), (API_KEY_VAR, None)]);
        // `.map(|_| ())` because the provider deliberately has no `Debug` —
        // it holds the API key, and a derived impl would print it.
        let err = SttProvider::from_env()
            .map(|_| ())
            .expect_err("missing key must fail");
        assert!(err.to_string().contains(API_KEY_VAR), "{err}");

        let _guard = EnvGuard::set(&[(BACKEND_VAR, Some("openai")), (API_KEY_VAR, Some("   "))]);
        assert!(
            SttProvider::from_env().is_err(),
            "an empty key is not a key"
        );
    }

    #[test]
    fn an_unknown_backend_names_the_valid_choices() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[(BACKEND_VAR, Some("whisper")), (API_KEY_VAR, Some("k"))]);
        let err = SttProvider::from_env()
            .map(|_| ())
            .expect_err("unknown backend must fail");
        let message = err.to_string();
        assert!(message.contains("openai"), "{message}");
        assert!(message.contains("deepgram"), "{message}");
    }

    #[test]
    fn known_audio_types_map_to_their_extension() {
        assert_eq!(extension_for("audio/mp4"), "mp4");
        assert_eq!(extension_for("audio/wav"), "wav");
        assert_eq!(extension_for("audio/ogg"), "ogg");
    }

    /// Browsers append codec parameters, e.g. `audio/webm;codecs=opus`.
    #[test]
    fn codec_parameters_are_ignored() {
        assert_eq!(extension_for("audio/webm;codecs=opus"), "webm");
        assert_eq!(extension_for("audio/mp4; codecs=mp4a.40.2"), "mp4");
    }

    #[test]
    fn unknown_types_fall_back_to_webm() {
        assert_eq!(extension_for("application/octet-stream"), "webm");
        assert_eq!(extension_for(""), "webm");
    }
}
