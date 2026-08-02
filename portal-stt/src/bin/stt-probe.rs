//! Exercise one speech-to-text provider end to end, without the portal.
//!
//! Network code cannot be unit-tested, so this is how a provider gets verified:
//! point it at the real API and see what comes back. Two useful modes —
//!
//! * with valid credentials and an audio file, it prints the transcript;
//! * with deliberately invalid credentials, a provider-level auth error proves
//!   the URL, headers and body shape reached the vendor and were understood.
//!   A transport error or a 404 means we built the request wrong.
//!
//! ```text
//! PORTAL_STT_BACKEND=deepgram PORTAL_STT_API_KEY=… \
//!     cargo run -p portal-stt --bin stt-probe -- recording.webm
//! ```
//!
//! With no file argument it sends a fraction of a second of silence, which is
//! enough to get past request validation and reach authentication.

use portal_stt::{SttEnv, SttProvider, TranscribeRequest, BACKENDS};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let audio_path = args.next();

    let backend = match std::env::var("PORTAL_STT_BACKEND") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_ascii_lowercase(),
        _ => {
            eprintln!(
                "set PORTAL_STT_BACKEND to one of: {}\n\
                 usage: stt-probe [audio-file]",
                BACKENDS.join(", ")
            );
            return std::process::ExitCode::from(2);
        }
    };

    let (audio, content_type) = match &audio_path {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => (bytes, content_type_for(path)),
            Err(error) => {
                eprintln!("could not read {path}: {error}");
                return std::process::ExitCode::from(2);
            }
        },
        None => (silent_wav(), "audio/wav"),
    };

    let provider = match SttProvider::build(&backend, &SttEnv::from_process()) {
        Ok(provider) => provider,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return std::process::ExitCode::from(2);
        }
    };

    let keyterms = portal_stt::session_keyterms(
        "/home/dev/repos/agent-portal",
        Some("meawoppl/stt-all-providers"),
        Some("https://github.com/meawoppl/agent-portal.git"),
        "claude",
    );
    println!(
        "provider={} audio={} bytes ({content_type}) keyterms={}",
        provider.key(),
        audio.len(),
        if provider.supports_keyterms() {
            keyterms.len().to_string()
        } else {
            "unsupported".to_string()
        },
    );

    let request = TranscribeRequest {
        audio: audio.into(),
        content_type,
        language: None,
        keyterms: if provider.supports_keyterms() {
            &keyterms
        } else {
            &[]
        },
    };

    match provider.transcribe(request).await {
        Ok(text) => {
            println!("OK: {text:?}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            println!("ERR[{}]: {error}", error_kind(&error));
            std::process::ExitCode::FAILURE
        }
    }
}

/// Which arm of `SttError` this is — the distinction that matters when reading
/// a probe run, since only `provider` proves we reached the vendor.
fn error_kind(error: &portal_stt::SttError) -> &'static str {
    match error {
        portal_stt::SttError::Provider(_) => "provider",
        portal_stt::SttError::Transport(_) => "transport",
        portal_stt::SttError::Decode(_) => "decode",
    }
}

fn content_type_for(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        _ => "audio/webm",
    }
}

/// A quarter-second of 16 kHz mono silence, as a complete RIFF/WAVE file.
///
/// Real audio would be better, but a well-formed file is all that is needed to
/// get past a provider's format validation and reach the auth check — and it
/// keeps the probe dependency-free and offline-safe.
fn silent_wav() -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const SAMPLES: u32 = SAMPLE_RATE / 4;
    let data_len = SAMPLES * 2;

    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM header size
    wav.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // channels: mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    wav
}
