//! Google Speech-to-Text Service
//!
//! Provides streaming speech recognition using Google Cloud Speech-to-Text API.

use google_cognitive_apis::api::grpc::google::cloud::speechtotext::v1::{
    streaming_recognize_request::StreamingRequest, RecognitionConfig, StreamingRecognitionConfig,
    StreamingRecognizeRequest,
};
use google_cognitive_apis::speechtotext::recognizer::Recognizer;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Audio encoding types supported by the speech service
#[derive(Debug, Clone, Copy)]
pub enum AudioEncoding {
    /// Linear PCM 16-bit signed little-endian
    Linear16,
}

impl From<AudioEncoding> for i32 {
    fn from(encoding: AudioEncoding) -> i32 {
        match encoding {
            AudioEncoding::Linear16 => 1, // LINEAR16 in Google's enum
        }
    }
}

/// Configuration for the speech recognition service
#[derive(Debug, Clone)]
pub struct SpeechConfig {
    /// Path to Google Cloud service account credentials JSON file
    pub credentials_path: Option<String>,
    /// Sample rate in Hz (default: 16000)
    pub sample_rate_hertz: i32,
    /// Language code (default: "en-US")
    pub language_code: String,
    /// Audio encoding (default: Linear16)
    pub encoding: AudioEncoding,
    /// Enable interim results during recognition
    pub interim_results: bool,
    /// Enable single utterance mode - auto-end recognition when speaker stops
    pub single_utterance: bool,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            credentials_path: None,
            sample_rate_hertz: 16000,
            language_code: "en-US".to_string(),
            encoding: AudioEncoding::Linear16,
            interim_results: true,
            single_utterance: true, // Auto-end when speaker stops, sends final result immediately
        }
    }
}

/// Result from speech recognition
#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    /// The transcribed text
    pub transcript: String,
    /// Whether this is a final result (vs interim)
    pub is_final: bool,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f32,
}

/// Speech-to-text service using Google Cloud
pub struct SpeechService {
    config: SpeechConfig,
}

impl SpeechService {
    /// Create a new speech service with the given configuration
    pub fn new(config: SpeechConfig) -> Self {
        Self { config }
    }

    /// Start a streaming recognition session
    ///
    /// Returns a tuple of:
    /// - A sender to push audio data (PCM16 bytes)
    /// - A receiver to get transcription results
    ///
    /// The session ends when the audio sender is dropped.
    pub async fn start_streaming(
        &self,
        language_code: Option<String>,
    ) -> Result<
        (
            mpsc::UnboundedSender<Vec<u8>>,
            mpsc::UnboundedReceiver<TranscriptionResult>,
        ),
        String,
    > {
        let credentials_path = self
            .config
            .credentials_path
            .clone()
            .ok_or_else(|| "Google Cloud credentials not configured".to_string())?;

        let language = language_code.unwrap_or_else(|| self.config.language_code.clone());

        // Create recognition config
        let recognition_config = RecognitionConfig {
            encoding: self.config.encoding.into(),
            sample_rate_hertz: self.config.sample_rate_hertz,
            language_code: language,
            enable_automatic_punctuation: true,
            ..Default::default()
        };

        let streaming_config = StreamingRecognitionConfig {
            config: Some(recognition_config),
            interim_results: self.config.interim_results,
            single_utterance: self.config.single_utterance,
        };

        // Create channels for audio input and transcription output
        let (audio_tx, audio_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (result_tx, result_rx) = mpsc::unbounded_channel::<TranscriptionResult>();

        // Spawn the recognition task
        let credentials = credentials_path.clone();
        tokio::spawn(async move {
            match run_recognition(credentials, streaming_config, audio_rx, result_tx).await {
                Ok(()) => info!("Speech recognition session completed"),
                Err(e) => error!("Speech recognition error: {}", e),
            }
        });

        Ok((audio_tx, result_rx))
    }
}

/// Run the actual speech recognition session
async fn run_recognition(
    credentials_path: String,
    config: StreamingRecognitionConfig,
    mut audio_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    result_tx: mpsc::UnboundedSender<TranscriptionResult>,
) -> Result<(), String> {
    // Read credentials JSON content from file
    let credentials_json = tokio::fs::read_to_string(&credentials_path)
        .await
        .map_err(|e| {
            format!(
                "Failed to read credentials file '{}': {}",
                credentials_path, e
            )
        })?;

    // Create recognizer with credentials JSON content
    let mut recognizer =
        Recognizer::create_streaming_recognizer(credentials_json, config, Some(1000))
            .await
            .map_err(|e| format!("Failed to create recognizer: {:?}", e))?;

    // Get the audio sink - using take_audio_sink() so it drops when we're done
    let audio_sink = recognizer
        .take_audio_sink()
        .ok_or_else(|| "Failed to get audio sink".to_string())?;

    // Get the result stream receiver before starting streaming
    let mut result_receiver = recognizer.get_streaming_result_receiver(Some(1000));

    // Spawn task to receive audio and send to recognizer
    let audio_task = tokio::spawn(async move {
        while let Some(audio_data) = audio_rx.recv().await {
            // Wrap raw bytes in StreamingRecognizeRequest
            let request = StreamingRecognizeRequest {
                streaming_request: Some(StreamingRequest::AudioContent(audio_data)),
            };
            if audio_sink.send(request).await.is_err() {
                warn!("Audio sink closed, stopping audio forwarding");
                break;
            }
        }
        // Dropping audio_sink signals end of audio stream
        drop(audio_sink);
    });

    // Spawn task to actually call the streaming recognize API
    let recognize_task = tokio::spawn(async move {
        // Start the streaming recognition
        if let Err(e) = recognizer.streaming_recognize().await {
            error!("Streaming recognize error: {:?}", e);
        }
    });

    // Process recognition results
    while let Some(response) = result_receiver.recv().await {
        // Concatenate all results to get the full transcript
        // Google splits responses into stable (high stability) and unstable (low stability) portions
        let mut full_transcript = String::new();
        let mut is_final = false;
        let mut max_confidence = 0.0f32;

        for result in &response.results {
            if let Some(alt) = result.alternatives.first() {
                full_transcript.push_str(&alt.transcript);
                if alt.confidence > max_confidence {
                    max_confidence = alt.confidence;
                }
            }
            if result.is_final {
                is_final = true;
            }
        }

        if !full_transcript.is_empty() {
            let transcription = TranscriptionResult {
                transcript: full_transcript,
                is_final,
                confidence: max_confidence,
            };

            if result_tx.send(transcription).is_err() {
                warn!("Result receiver closed, stopping recognition");
                break;
            }
        }
    }

    // Wait for tasks to complete
    let _ = audio_task.await;
    let _ = recognize_task.await;

    Ok(())
}
