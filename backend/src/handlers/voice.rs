//! Voice WebSocket Handler
//!
//! Handles audio streaming for voice-to-text functionality.
//! Audio is received as binary PCM16 frames and forwarded to
//! Google Speech-to-Text for transcription.

use crate::speech::{SpeechConfig, SpeechService};
use crate::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use diesel::prelude::*;
use futures_util::{SinkExt, StreamExt};
use shared::VoiceMessage;
use std::sync::Arc;
use tokio::sync::mpsc;
use tower_cookies::Cookies;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Check if user has voice feature enabled
fn check_voice_enabled(app_state: &AppState, user_id: Uuid) -> bool {
    let mut conn = match app_state.db_pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to get database connection for voice check: {}", e);
            return false;
        }
    };

    use crate::schema::users;
    users::table
        .filter(users::id.eq(user_id))
        .select(users::voice_enabled)
        .first::<bool>(&mut conn)
        .unwrap_or(false)
}

/// Verify that a user has access to a session (is a member with any role)
fn verify_session_access(app_state: &AppState, session_id: Uuid, user_id: Uuid) -> bool {
    let mut conn = match app_state.db_pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!(
                "Failed to get database connection for voice session access: {}",
                e
            );
            return false;
        }
    };

    use crate::schema::{session_members, sessions};
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .count()
        .get_result::<i64>(&mut conn)
        .unwrap_or(0)
        > 0
}

/// WebSocket endpoint for voice audio streaming
///
/// Route: /ws/voice/:session_id
///
/// This endpoint accepts binary audio frames (PCM16, 16kHz mono) and
/// streams them to the speech-to-text service. Transcription results
/// are sent back as JSON messages.
pub async fn handle_voice_websocket(
    ws: WebSocketUpgrade,
    Path(session_id): Path<Uuid>,
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Response {
    // Authenticate the user
    let user_id = match crate::auth::extract_user_id(&app_state, &cookies).ok() {
        Some(id) => id,
        None => {
            warn!("Unauthenticated voice WebSocket connection attempt");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    // Check if user has voice feature enabled
    if !check_voice_enabled(&app_state, user_id) {
        warn!(
            "User {} attempted voice WebSocket but voice is not enabled",
            user_id
        );
        return StatusCode::FORBIDDEN.into_response();
    }

    // Verify session access
    if !verify_session_access(&app_state, session_id, user_id) {
        warn!(
            "User {} attempted voice WebSocket for session {} they don't have access to",
            user_id, session_id
        );
        return StatusCode::FORBIDDEN.into_response();
    }

    // Check if speech credentials are configured
    let speech_credentials = app_state.speech_credentials_path.clone();

    info!(
        "Voice WebSocket upgrade for user {} on session {}",
        user_id, session_id
    );
    ws.on_upgrade(move |socket| {
        handle_voice_socket(socket, user_id, session_id, speech_credentials)
    })
}

/// State for an active voice recognition session
struct VoiceRecognitionSession {
    audio_tx: mpsc::UnboundedSender<Vec<u8>>,
}

async fn handle_voice_socket(
    socket: WebSocket,
    user_id: Uuid,
    session_id: Uuid,
    speech_credentials: Option<String>,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    info!(
        "Voice WebSocket connected for user {} on session {}",
        user_id, session_id
    );

    // Channel for sending messages back to the client
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<VoiceMessage>();

    // Spawn task to send messages to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_sender.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Current recognition session (if any)
    let mut recognition_session: Option<VoiceRecognitionSession> = None;

    // Handle incoming messages
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                // Binary audio data (PCM16, 16kHz mono)
                if let Some(ref session) = recognition_session {
                    // Forward to speech recognition
                    if session.audio_tx.send(data.to_vec()).is_err() {
                        warn!("Speech recognition session closed unexpectedly");
                        recognition_session = None;
                    }
                } else {
                    warn!(
                        "Received audio data but no recognition session active for {}",
                        session_id
                    );
                }
            }
            Ok(Message::Text(text)) => {
                // Handle control messages (JSON)
                if let Ok(proxy_msg) = serde_json::from_str::<VoiceMessage>(&text) {
                    match proxy_msg {
                        VoiceMessage::StartVoice {
                            session_id: msg_session_id,
                            language_code,
                        } => {
                            if msg_session_id != session_id {
                                warn!("StartVoice session_id mismatch");
                                continue;
                            }

                            // Stop any existing session
                            recognition_session = None;

                            info!(
                                "Starting voice recognition for session {} with language {}",
                                session_id, language_code
                            );

                            // Check if speech credentials are configured
                            let credentials = match &speech_credentials {
                                Some(path) => path.clone(),
                                None => {
                                    let error_msg = VoiceMessage::VoiceError {
                                        session_id,
                                        message: "Speech-to-text not configured on server"
                                            .to_string(),
                                    };
                                    let _ = client_tx.send(error_msg);
                                    continue;
                                }
                            };

                            // Create speech service with config
                            let config = SpeechConfig {
                                credentials_path: Some(credentials),
                                language_code: language_code.clone(),
                                ..Default::default()
                            };
                            let speech_service = SpeechService::new(config);

                            // Start streaming recognition
                            match speech_service.start_streaming(Some(language_code)).await {
                                Ok((audio_tx, mut result_rx)) => {
                                    recognition_session =
                                        Some(VoiceRecognitionSession { audio_tx });

                                    // Spawn task to forward transcription results to client
                                    let client_tx_clone = client_tx.clone();
                                    tokio::spawn(async move {
                                        while let Some(result) = result_rx.recv().await {
                                            info!(
                                                "Forwarding to WebSocket: is_final={}, transcript=\"{}\"",
                                                result.is_final, result.transcript
                                            );
                                            let msg = VoiceMessage::Transcription {
                                                session_id,
                                                transcript: result.transcript,
                                                is_final: result.is_final,
                                                confidence: result.confidence,
                                            };
                                            if client_tx_clone.send(msg).is_err() {
                                                info!("Client disconnected, stopping transcription forwarding");
                                                break;
                                            }
                                        }
                                        // Recognition stream ended (e.g., single_utterance detected end of speech)
                                        // Signal the frontend to stop recording
                                        let _ = client_tx_clone
                                            .send(VoiceMessage::VoiceEnded { session_id });
                                        info!(
                                            "Speech recognition stream ended for session {}",
                                            session_id
                                        );
                                    });

                                    info!("Speech recognition session started for {}", session_id);
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to start speech recognition for {}: {}",
                                        session_id, e
                                    );
                                    let error_msg = VoiceMessage::VoiceError {
                                        session_id,
                                        message: format!(
                                            "Failed to start speech recognition: {}",
                                            e
                                        ),
                                    };
                                    let _ = client_tx.send(error_msg);
                                }
                            }
                        }
                        VoiceMessage::StopVoice {
                            session_id: msg_session_id,
                        } => {
                            if msg_session_id != session_id {
                                warn!("StopVoice session_id mismatch");
                                continue;
                            }
                            info!("Stopping voice recognition for session {}", session_id);
                            // Dropping the session will close the audio channel
                            recognition_session = None;
                        }
                        _ => {
                            warn!("Unexpected message type on voice WebSocket");
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                info!("Voice WebSocket closed for session {}", session_id);
                break;
            }
            Ok(Message::Ping(_)) => {
                // Pong is handled automatically by axum
            }
            Err(e) => {
                error!("Voice WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    // Cleanup
    drop(recognition_session);
    send_task.abort();

    info!(
        "Voice WebSocket disconnected for user {} on session {}",
        user_id, session_id
    );
}
