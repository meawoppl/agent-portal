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

mod config;
mod http;
mod keyterms;
mod poll;
mod providers;

pub use config::{SttEnv, BACKEND_VAR};
pub use keyterms::session_keyterms;

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

/// The message from a provider's `from_env` failure.
///
/// Providers deliberately do not derive `Debug` — they hold API keys, and a
/// derived impl would print them — so tests cannot call `unwrap_err` directly.
#[cfg(test)]
pub(crate) fn config_error<T>(result: anyhow::Result<T>) -> String {
    result
        .map(|_| ())
        .expect_err("expected a configuration error")
        .to_string()
}

/// A configured speech-to-text provider.
///
/// Opaque on the outside — callers only need [`key`](Self::key),
/// [`supports_keyterms`](Self::supports_keyterms) and
/// [`transcribe`](Self::transcribe) — and a closed enum on the inside, so
/// adding a vendor is a compile error in every arm that has to learn about it.
#[derive(Clone)]
pub struct SttProvider {
    inner: Inner,
}

#[derive(Clone)]
enum Inner {
    AssemblyAi(providers::assemblyai::AssemblyAiStt),
    Aws(providers::aws::AwsStt),
    Azure(providers::azure::AzureStt),
    Deepgram(providers::deepgram::DeepgramStt),
    Google(providers::google::GoogleStt),
    Ibm(providers::ibm::IbmStt),
    OpenAi(providers::openai::OpenAiStt),
    RevAi(providers::revai::RevAiStt),
    Simplismart(providers::simplismart::SimplismartStt),
    Speechmatics(providers::speechmatics::SpeechmaticsStt),
}

/// Every backend name accepted by `PORTAL_STT_BACKEND`, for error messages and
/// for the probe tool. Kept adjacent to [`SttProvider::build`] so the two do not
/// drift.
pub const BACKENDS: &[&str] = &[
    "assemblyai",
    "aws",
    "azure",
    "deepgram",
    "google",
    "ibm",
    "openai",
    "revai",
    "simplismart",
    "speechmatics",
];

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
        Self::build(&backend, &SttEnv::from_process()).map(Some)
    }

    /// Construct one provider by name from an explicit environment. Separate
    /// from [`Self::from_env`] so it is reachable without mutating process
    /// globals.
    pub fn build(backend: &str, env: &SttEnv) -> anyhow::Result<Self> {
        let inner = match backend {
            "assemblyai" => Inner::AssemblyAi(providers::assemblyai::AssemblyAiStt::from_env(env)?),
            "aws" => Inner::Aws(providers::aws::AwsStt::from_env(env)?),
            "azure" => Inner::Azure(providers::azure::AzureStt::from_env(env)?),
            "deepgram" => Inner::Deepgram(providers::deepgram::DeepgramStt::from_env(env)?),
            "google" => Inner::Google(providers::google::GoogleStt::from_env(env)?),
            "ibm" => Inner::Ibm(providers::ibm::IbmStt::from_env(env)?),
            "openai" => Inner::OpenAi(providers::openai::OpenAiStt::from_env(env)?),
            "revai" => Inner::RevAi(providers::revai::RevAiStt::from_env(env)?),
            "simplismart" => {
                Inner::Simplismart(providers::simplismart::SimplismartStt::from_env(env)?)
            }
            "speechmatics" => {
                Inner::Speechmatics(providers::speechmatics::SpeechmaticsStt::from_env(env)?)
            }
            other => anyhow::bail!(
                "unknown {BACKEND_VAR}={other:?}; expected disabled or one of: {}",
                BACKENDS.join(", ")
            ),
        };
        Ok(Self { inner })
    }

    /// Stable key for logs and `/api/config`.
    pub fn key(&self) -> &'static str {
        match &self.inner {
            Inner::AssemblyAi(_) => "assemblyai",
            Inner::Aws(_) => "aws",
            Inner::Azure(_) => "azure",
            Inner::Deepgram(_) => "deepgram",
            Inner::Google(_) => "google",
            Inner::Ibm(_) => "ibm",
            Inner::OpenAi(_) => "openai",
            Inner::RevAi(_) => "revai",
            Inner::Simplismart(_) => "simplismart",
            Inner::Speechmatics(_) => "speechmatics",
        }
    }

    /// Whether this provider can bias decoding with [`session_keyterms`].
    ///
    /// Not every vendor exposes a per-request vocabulary: Azure, IBM and
    /// Simplismart need a trained model instead, and AWS needs a pre-created
    /// named vocabulary. Callers use this to avoid computing hints nobody will
    /// read, and it keeps the "which providers actually bias?" answer in one
    /// place rather than scattered through the modules.
    pub fn supports_keyterms(&self) -> bool {
        match &self.inner {
            Inner::AssemblyAi(_)
            | Inner::Deepgram(_)
            | Inner::Google(_)
            | Inner::OpenAi(_)
            | Inner::RevAi(_)
            | Inner::Speechmatics(_) => true,
            Inner::Aws(_) | Inner::Azure(_) | Inner::Ibm(_) | Inner::Simplismart(_) => false,
        }
    }

    pub async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<String, SttError> {
        match &self.inner {
            Inner::AssemblyAi(provider) => provider.transcribe(request).await,
            Inner::Aws(provider) => provider.transcribe(request).await,
            Inner::Azure(provider) => provider.transcribe(request).await,
            Inner::Deepgram(provider) => provider.transcribe(request).await,
            Inner::Google(provider) => provider.transcribe(request).await,
            Inner::Ibm(provider) => provider.transcribe(request).await,
            Inner::OpenAi(provider) => provider.transcribe(request).await,
            Inner::RevAi(provider) => provider.transcribe(request).await,
            Inner::Simplismart(provider) => provider.transcribe(request).await,
            Inner::Speechmatics(provider) => provider.transcribe(request).await,
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
    use crate::config::{API_KEY_VAR, MODEL_VAR};

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
