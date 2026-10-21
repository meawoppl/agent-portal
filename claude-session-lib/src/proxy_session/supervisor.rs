//! Process-lifetime connection supervision. Transport registration and the live
//! message loop stay in the parent; this module owns retry policy and replay.

use super::git_metadata::{get_open_prs, get_pr_url};
use super::{
    connect_to_backend, get_git_branch, get_repo_url, hostname_or_unknown, register_session,
    run_message_loop, ConnectionResult, PortalInput, ProxySessionConfig, RegisterError,
};
use anyhow::Result;
use session_lib::{agent::Agent, output_buffer::PendingOutputBuffer, session::Session};
use shared::ProxyToServer;
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

/// Exponential backoff helper
pub struct Backoff {
    current: u64,
    initial: u64,
    max: u64,
    multiplier: u64,
    stable_threshold: u64,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            current: 1,
            initial: 1,
            max: shared::protocol::MAX_RECONNECT_BACKOFF_SECS,
            multiplier: 2,
            stable_threshold: 30,
        }
    }

    /// Get the current backoff duration
    pub fn current_secs(&self) -> u64 {
        self.current
    }

    /// Advance to the next backoff interval
    pub fn advance(&mut self) {
        self.current = (self.current * self.multiplier).min(self.max);
    }

    /// Reset backoff if connection was stable
    pub fn reset_if_stable(&mut self, connection_duration: Duration) {
        if connection_duration.as_secs() >= self.stable_threshold {
            info!(
                "Connection was stable for {}s, resetting backoff",
                connection_duration.as_secs()
            );
            self.current = self.initial;
        }
    }

    /// Reset backoff to initial value unconditionally
    pub fn reset(&mut self) {
        self.current = self.initial;
    }

    /// Get a sleep duration
    pub fn sleep_duration(&self) -> Duration {
        Duration::from_secs(self.current)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Result from the connection loop
#[derive(Debug, PartialEq, Eq)]
pub enum LoopResult {
    /// Normal exit (agent process ended)
    NormalExit,
    /// Session not found - caller should restart with fresh session
    SessionNotFound,
    /// Registration was rejected by the backend. The launcher should let the
    /// process exit; reconcile will relaunch it with a freshly minted token
    /// (#1045).
    RegistrationRejected,
}

/// State that persists across WebSocket reconnections for a session.
/// This includes the input channel, output buffer, and session config.
pub struct SessionState<'a, A: Agent> {
    /// Session configuration
    pub config: &'a ProxySessionConfig,
    /// The agent-backed session (Claude or Codex).
    pub claude_session: &'a mut Session<A>,
    /// Sender for input messages (cloned per connection)
    pub input_tx: mpsc::UnboundedSender<PortalInput>,
    /// Receiver for input messages (persists across connections)
    pub input_rx: &'a mut mpsc::UnboundedReceiver<PortalInput>,
    /// Output buffer with persistence
    pub output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    /// Backoff state for reconnection
    pub backoff: Backoff,
    /// Whether this is the first connection attempt
    pub first_connection: bool,
    /// Whether the portal-features reminder still needs to ride along with the
    /// next user input. Lives here, not on `ConnectionState`, because it is
    /// scoped to the agent PROCESS: a WS reconnect must not re-send it, but a
    /// respawned agent must get it again. Shared with the per-connection state
    /// so the input arms can clear it without a write-back.
    pub reminder_pending: Arc<AtomicBool>,
    /// When the last disconnect occurred (for reporting reconnect duration)
    pub disconnected_at: Option<Instant>,
    /// Whether the last disconnect was a graceful server shutdown
    pub last_disconnect_graceful: bool,
    /// Consecutive replay failures on the same pending messages
    pub replay_failures: u32,
}

impl<'a, A: Agent> SessionState<'a, A> {
    /// Create a new session state
    pub fn new(
        config: &'a ProxySessionConfig,
        claude_session: &'a mut Session<A>,
        input_tx: mpsc::UnboundedSender<PortalInput>,
        input_rx: &'a mut mpsc::UnboundedReceiver<PortalInput>,
    ) -> Result<Self> {
        let output_buffer = Arc::new(Mutex::new(PendingOutputBuffer::new(config.session_id)?));

        Ok(Self {
            config,
            claude_session,
            input_tx,
            input_rx,
            output_buffer,
            backoff: Backoff::new(),
            first_connection: true,
            reminder_pending: Arc::new(AtomicBool::new(
                config.prime_portal_reminder_on_first_input,
            )),
            disconnected_at: None,
            last_disconnect_graceful: false,
            replay_failures: 0,
        })
    }

    /// Log pending messages from previous session
    pub async fn log_pending_messages(&self) {
        let buf = self.output_buffer.lock().await;
        let pending = buf.pending_count();
        if pending > 0 {
            info!(
                "Loaded {} pending messages from previous session, will replay on connect",
                pending
            );
        }
    }

    /// Persist the output buffer
    pub async fn persist_buffer(&self) {
        if let Err(e) = self.output_buffer.lock().await.persist() {
            warn!("Failed to persist output buffer: {}", e);
        }
    }

    /// Get pending message count
    pub async fn pending_count(&self) -> usize {
        self.output_buffer.lock().await.pending_count()
    }
}

/// Record the start of an outage, classifying it **once**.
///
/// Both the "connection dropped" and "reconnect attempt failed" paths report
/// `ConnectionResult::Disconnected`, and every retry during a backend restart
/// takes the latter. Classifying on each call let those retries overwrite a
/// graceful classification, so any restart outlasting the first backoff — i.e.
/// every real redeploy — was reported to the user as an "unexpected
/// disconnect". Only the first call after a live connection is lost decides
/// what kind of outage this is; later calls just keep waiting.
fn note_disconnect(
    disconnected_at: &mut Option<Instant>,
    last_disconnect_graceful: &mut bool,
    graceful: bool,
) {
    if disconnected_at.is_none() {
        *disconnected_at = Some(Instant::now());
        *last_disconnect_graceful = graceful;
    }
}

/// A connection attempt's policy decision, independent of sockets and timers.
/// `advance` is applied only after sleeping; a server-requested pause resets
/// exponential backoff without advancing it.
#[derive(Debug, PartialEq, Eq)]
enum Transition {
    Stop(LoopResult),
    Retry { delay: Duration, advance: bool },
}

fn transition_after_connection(
    result: ConnectionResult,
    backoff: &mut Backoff,
    disconnected_at: &mut Option<Instant>,
    graceful: &mut bool,
) -> Transition {
    match result {
        ConnectionResult::AgentExited => {
            info!("Agent process exited, shutting down");
            Transition::Stop(LoopResult::NormalExit)
        }
        ConnectionResult::SessionTerminated => {
            info!("Session terminated by server, not reconnecting");
            Transition::Stop(LoopResult::NormalExit)
        }
        ConnectionResult::SessionNotFound => {
            warn!("Session not found, need to restart with fresh session");
            Transition::Stop(LoopResult::SessionNotFound)
        }
        ConnectionResult::RegistrationRejected => {
            error!("Registration rejected by server; not reconnecting with these credentials");
            Transition::Stop(LoopResult::RegistrationRejected)
        }
        ConnectionResult::Disconnected(duration) => {
            note_disconnect(disconnected_at, graceful, false);
            backoff.reset_if_stable(duration);
            Transition::Retry {
                delay: backoff.sleep_duration(),
                advance: true,
            }
        }
        ConnectionResult::ServerShutdown(delay) => {
            note_disconnect(disconnected_at, graceful, true);
            backoff.reset();
            Transition::Retry {
                delay,
                advance: false,
            }
        }
    }
}

/// Run the WebSocket connection loop with auto-reconnect
pub async fn run_connection_loop<A: Agent>(
    config: &ProxySessionConfig,
    claude_session: &mut Session<A>,
    input_tx: mpsc::UnboundedSender<PortalInput>,
    input_rx: &mut mpsc::UnboundedReceiver<PortalInput>,
) -> Result<LoopResult> {
    let mut session = SessionState::new(config, claude_session, input_tx, input_rx)?;
    session.log_pending_messages().await;

    loop {
        if session.first_connection {
            info!("Proxy ready");
        }

        let result = run_single_connection(&mut session).await;
        session.first_connection = false;

        let transition = transition_after_connection(
            result,
            &mut session.backoff,
            &mut session.disconnected_at,
            &mut session.last_disconnect_graceful,
        );
        // Persist on every exit path, before sleeping or handing control back
        // to the launcher. No connection result is allowed to bypass this.
        session.persist_buffer().await;
        match transition {
            Transition::Stop(result) => return Ok(result),
            Transition::Retry { delay, advance } => {
                let pending = session.pending_count().await;
                if advance {
                    warn!(
                        "WebSocket disconnected, {} pending messages, reconnecting in {}s",
                        pending,
                        delay.as_secs()
                    );
                } else {
                    info!(
                        "Server shutting down, {} pending messages, reconnecting in {}s",
                        pending,
                        delay.as_secs().max(1)
                    );
                }
                tokio::time::sleep(delay).await;
                if advance {
                    session.backoff.advance();
                }
            }
        }
    }
}

/// Run a single WebSocket connection until it disconnects or Claude exits
async fn run_single_connection<A: Agent>(session: &mut SessionState<'_, A>) -> ConnectionResult {
    // Connect to WebSocket
    let mut conn =
        match connect_to_backend(&session.config.backend_url, session.first_connection).await {
            Ok(conn) => conn,
            Err(duration) => return ConnectionResult::Disconnected(duration),
        };

    // Re-detect git branch on reconnect (it may have changed)
    let current_branch = get_git_branch(&session.config.working_directory);
    let config_with_branch = ProxySessionConfig {
        git_branch: current_branch,
        ..session.config.clone()
    };

    // Register with backend and wait for acknowledgment. A ticket in the ack
    // means we may open the binary port-forward data plane (#1506).
    let tunnel_data_grant = match register_session(&mut conn, &config_with_branch).await {
        Ok(grant) => grant,
        Err(RegisterError::Transient(duration)) => return ConnectionResult::Disconnected(duration),
        Err(RegisterError::Rejected) => return ConnectionResult::RegistrationRejected,
    };

    // Look up PR URL, repo URL, and the repo's open PRs for the current branch
    // and send as the initial SessionUpdate (so the pill populates immediately).
    let repo_url = get_repo_url(&session.config.working_directory);
    let pr_url = config_with_branch
        .git_branch
        .as_deref()
        .and_then(|b| get_pr_url(&session.config.working_directory, b));
    let open_prs = get_open_prs(&session.config.working_directory);
    let update_msg = ProxyToServer::SessionUpdate {
        session_id: config_with_branch.session_id,
        git_branch: config_with_branch.git_branch.clone(),
        pr_url,
        repo_url,
        open_prs,
    };
    if conn.send(update_msg).await.is_err() {
        error!("Failed to send initial session update");
    }

    // Replay pending messages after successful registration.
    // If replay fails repeatedly (e.g., backend reset and rejects stale seqs),
    // drop the pending messages after MAX_REPLAY_FAILURES to avoid an infinite loop.
    const MAX_REPLAY_FAILURES: u32 = 5;
    {
        let mut buf = session.output_buffer.lock().await;
        let pending_count = buf.pending_count();
        if pending_count > 0 {
            if session.replay_failures >= MAX_REPLAY_FAILURES {
                error!(
                    "Dropping {} pending messages after {} consecutive replay failures",
                    pending_count, session.replay_failures
                );
                buf.clear();
                session.replay_failures = 0;
            } else {
                debug!(
                    "Replaying {} pending messages after reconnect (attempt {})",
                    pending_count,
                    session.replay_failures + 1
                );
                let mut replay_failed = false;
                for pending in buf.get_pending() {
                    let msg = ProxyToServer::SequencedOutput {
                        seq: pending.seq,
                        content: pending.content.clone(),
                        agent_type: config_with_branch.agent_type,
                    };
                    if conn.send(msg).await.is_err() {
                        error!("Failed to replay pending message seq={}", pending.seq);
                        session.replay_failures += 1;
                        replay_failed = true;
                        break;
                    }
                }
                if replay_failed {
                    return ConnectionResult::Disconnected(Duration::ZERO);
                }
                debug!("Finished replaying pending messages");
                session.replay_failures = 0;
            }
        }
    }

    // Announce the session once. Reconnects are routine transport churn and
    // only go to the log — posting them filled idle transcripts with walls of
    // "reconnected" lines.
    if session.first_connection {
        let short_id = shared::short_uuid(&session.config.session_id);
        let mut text = format!(
            "**Session started** — `{}` on `{}` in `{}` ({} `{}…`)",
            session.config.session_name,
            hostname_or_unknown(),
            session.config.working_directory,
            config_with_branch.agent_type,
            short_id,
        );
        if let Some(notice) = &session.config.startup_notice {
            text.push_str("\n\n");
            text.push_str(notice);
        }
        let portal_content = shared::PortalMessage::text(text).to_json();
        let seq = {
            let mut buf = session.output_buffer.lock().await;
            buf.push(portal_content.clone())
        };
        let msg = ProxyToServer::SequencedOutput {
            seq,
            content: portal_content,
            agent_type: config_with_branch.agent_type,
        };
        if conn.send(msg).await.is_err() {
            error!("Failed to send connection portal message");
            return ConnectionResult::Disconnected(Duration::ZERO);
        }
    } else {
        let after = session
            .disconnected_at
            .map(|t| format!(" after {}s", t.elapsed().as_secs()))
            .unwrap_or_default();
        let reason = if session.last_disconnect_graceful {
            "server restart"
        } else {
            "unexpected disconnect"
        };
        info!("Connection restored{after} ({reason})");
        session.disconnected_at = None;
    }

    // Run the message loop - split connection for concurrent read/write
    run_message_loop(session, &config_with_branch, conn, tunnel_data_grant).await
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The redeploy bug: the backend announces a restart (graceful), then every
    /// reconnect attempt fails while it is coming back up. Those retries must
    /// not relabel the outage — a planned redeploy that takes longer than one
    /// backoff was being reported to the user as an "unexpected disconnect".
    #[test]
    fn retries_during_a_restart_do_not_relabel_a_graceful_outage() {
        let (mut at, mut graceful) = (None, false);

        note_disconnect(&mut at, &mut graceful, true);
        assert!(graceful, "the server told us it was restarting");
        let first_seen = at;

        // 35 seconds of failed reconnect attempts, each reporting Disconnected.
        for _ in 0..8 {
            note_disconnect(&mut at, &mut graceful, false);
        }

        assert!(graceful, "retries must not turn a restart into a surprise");
        assert_eq!(at, first_seen, "the outage clock starts at the first drop");
    }

    /// The converse still has to work: a genuinely unexpected drop stays
    /// unexpected.
    #[test]
    fn an_unannounced_drop_stays_unexpected() {
        let (mut at, mut graceful) = (None, false);
        note_disconnect(&mut at, &mut graceful, false);
        assert!(!graceful);
        assert!(at.is_some());
    }

    #[test]
    fn backoff_doubles_then_saturates_at_max() {
        let mut b = Backoff::new();
        assert_eq!(b.current_secs(), 1);
        b.advance();
        assert_eq!(b.current_secs(), 2);
        b.advance();
        assert_eq!(b.current_secs(), 4);
        // Many advances must clamp at the protocol cap, never overflow past it.
        for _ in 0..40 {
            b.advance();
        }
        assert_eq!(
            b.current_secs(),
            shared::protocol::MAX_RECONNECT_BACKOFF_SECS
        );
    }

    #[test]
    fn backoff_reset_if_stable_only_resets_past_threshold() {
        let mut b = Backoff::new();
        b.advance();
        b.advance();
        // Below the 30s stability threshold: keep backing off.
        b.reset_if_stable(Duration::from_secs(29));
        assert_eq!(b.current_secs(), 4);
        // At/over the threshold: a stable connection earns a reset to initial.
        b.reset_if_stable(Duration::from_secs(30));
        assert_eq!(b.current_secs(), 1);
    }

    #[test]
    fn backoff_reset_is_unconditional() {
        let mut b = Backoff::new();
        b.advance();
        b.advance();
        b.reset();
        assert_eq!(b.current_secs(), 1);
    }

    #[test]
    fn terminal_results_never_retry_or_mutate_outage_state() {
        for (result, expected) in [
            (ConnectionResult::AgentExited, LoopResult::NormalExit),
            (ConnectionResult::SessionTerminated, LoopResult::NormalExit),
            (
                ConnectionResult::SessionNotFound,
                LoopResult::SessionNotFound,
            ),
            (
                ConnectionResult::RegistrationRejected,
                LoopResult::RegistrationRejected,
            ),
        ] {
            let mut backoff = Backoff::new();
            backoff.advance();
            let (mut at, mut graceful) = (None, false);
            assert_eq!(
                transition_after_connection(result, &mut backoff, &mut at, &mut graceful),
                Transition::Stop(expected)
            );
            assert_eq!(backoff.current_secs(), 2);
            assert!(at.is_none());
        }
    }

    #[test]
    fn restart_then_failed_attempts_keep_server_delay_and_original_outage() {
        let mut backoff = Backoff::new();
        backoff.advance();
        backoff.advance();
        let (mut at, mut graceful) = (None, false);
        let delay = Duration::from_millis(250);
        assert_eq!(
            transition_after_connection(
                ConnectionResult::ServerShutdown(delay),
                &mut backoff,
                &mut at,
                &mut graceful
            ),
            Transition::Retry {
                delay,
                advance: false
            }
        );
        assert_eq!(backoff.current_secs(), 1);
        let started = at;
        for seconds in [1, 2, 4, 8] {
            assert_eq!(
                transition_after_connection(
                    ConnectionResult::Disconnected(Duration::ZERO),
                    &mut backoff,
                    &mut at,
                    &mut graceful
                ),
                Transition::Retry {
                    delay: Duration::from_secs(seconds),
                    advance: true
                }
            );
            assert_eq!(at, started);
            assert!(graceful);
            backoff.advance();
        }
        // A genuinely stable connection resets the next retry, even after
        // several failed attempts. Reconnection clears the outage timestamp.
        at = None;
        assert_eq!(
            transition_after_connection(
                ConnectionResult::Disconnected(Duration::from_secs(30)),
                &mut backoff,
                &mut at,
                &mut graceful
            ),
            Transition::Retry {
                delay: Duration::from_secs(1),
                advance: true
            }
        );
        assert!(!graceful);
    }
}
