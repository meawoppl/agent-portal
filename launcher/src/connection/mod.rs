mod commands;
mod process_events;
mod scheduler_events;
mod supervisor;
use commands::handle_message;
use supervisor::LauncherConnectionSupervisor;

use crate::process_manager::{ProcessManager, SessionExited};
use crate::scheduler::Scheduler;
use shared::{LauncherEndpoint, LauncherRejectReason, LauncherToServer, ServerToLauncher};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

const HEARTBEAT_INTERVAL: Duration =
    Duration::from_secs(shared::protocol::LAUNCHER_HEARTBEAT_INTERVAL_SECS);
/// If the backend echoes our heartbeats (it advertises support by sending the
/// first ack) but then goes this long without one, the control socket is
/// half-open — force a reconnect. Three missed intervals tolerates a slow tick
/// or a dropped ack without flapping (#1366).
const HEARTBEAT_ACK_TIMEOUT: Duration =
    Duration::from_secs(shared::protocol::LAUNCHER_HEARTBEAT_INTERVAL_SECS * 3);
/// Cadence of `LauncherToServer::SystemStats` against a backend that accepts
/// it (`SERVER_CAPABILITY_SYSTEM_STATS`); the heartbeat carries a reading
/// regardless.
const SYSTEM_STATS_INTERVAL: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(shared::protocol::MAX_RECONNECT_BACKOFF_SECS);
/// Retry cadence while parked on a fatal rejection: start here, double to the
/// cap. Deliberately much slower than the network-error backoff — a fatal
/// rejection won't clear until something changes (a re-login, a launcher
/// stopping elsewhere), and hammering registration was the #1237 crash loop.
const PARKED_BACKOFF_START: Duration = Duration::from_secs(60);
const PARKED_BACKOFF_MAX: Duration = Duration::from_secs(600);

/// How the launcher should respond to a fatal registration rejection.
/// Prefers the backend's machine-readable reason; falls back to sniffing the
/// error text for old backends that predate `reject_reason`.
fn classify_fatal(reason: Option<LauncherRejectReason>, error_msg: &str) -> LauncherRejectReason {
    if let Some(reason) = reason {
        return reason;
    }
    if shared::strings::contains_case_insensitive(error_msg, "auth")
        || shared::strings::contains_case_insensitive(error_msg, "token")
    {
        LauncherRejectReason::AuthFailed
    } else if shared::strings::contains_case_insensitive(error_msg, "already have") {
        LauncherRejectReason::TooManyLaunchers
    } else {
        LauncherRejectReason::DuplicateLauncher
    }
}

/// Whether the control socket looks half-open and should be reconnected.
///
/// Only ever true once we've seen at least one heartbeat ack (`ack_seen`) —
/// against an older backend that never echoes, this stays `false` forever, so
/// the watchdog can't cause false reconnect loops on a healthy-but-old hub
/// (#1366).
fn control_socket_half_open(ack_seen: bool, since_last_ack: Duration) -> bool {
    ack_seen && since_last_ack > HEARTBEAT_ACK_TIMEOUT
}

/// Local-only reason: do not add a new wire enum value to a major-version
/// rejection that an older launcher must still be able to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParkedReason {
    Server(LauncherRejectReason),
    ProtocolIncompatible,
}

/// The one-line operator instruction for a parked launcher.
fn parked_instruction(reason: ParkedReason) -> &'static str {
    match reason {
        ParkedReason::ProtocolIncompatible => {
            "Incompatible protocol — update agent-portal and the Portal server to compatible releases. Retrying slowly."
        }
        ParkedReason::Server(LauncherRejectReason::AuthFailed) => {
            "Authentication failed — run `agent-portal login` to re-authenticate. \
             The launcher will keep retrying slowly and recover on its own once \
             you log in."
        }
        ParkedReason::Server(LauncherRejectReason::TooManyLaunchers) => {
            "Launcher limit reached — disconnect another launcher. Retrying slowly."
        }
        ParkedReason::Server(LauncherRejectReason::DuplicateLauncher) => {
            "Another launcher is already connected from this host — stop it (or \
             this one). Retrying slowly."
        }
    }
}

pub async fn run_launcher_loop(
    backend_url: &str,
    launcher_id: Uuid,
    launcher_name: &str,
    cli_auth_token: Option<String>,
    mut process_manager: ProcessManager,
    mut exit_rx: mpsc::UnboundedReceiver<SessionExited>,
) -> anyhow::Result<()> {
    process_manager.set_launcher_id(launcher_id);
    // Service ownership outlives any session and backend reconnect. Aborting
    // this task on launcher shutdown drops its children and relay listeners.
    let services = tokio::spawn(async {
        if let Err(err) = crate::managed_forward::supervise().await {
            warn!(error = %err, "managed-service supervisor stopped");
        }
    });
    let _services = crate::managed_forward::Task(services);
    let mut backoff = Duration::from_secs(1);
    let mut parked_backoff = PARKED_BACKOFF_START;
    // The reason whose instruction was last shown; re-emit when it changes so
    // an operator who fixes auth and then hits the launcher limit sees the new
    // guidance, not just "Still parked".
    let mut last_parked_reason: Option<ParkedReason> = None;
    let mut scheduler = Scheduler::new();
    let mut supervisor = LauncherConnectionSupervisor::default();
    let mut login_registry = crate::login_registry::LoginRegistry::new();

    loop {
        // Re-resolve the token every attempt (CLI flag still wins): a parked
        // launcher self-heals the moment `agent-portal login` writes a fresh
        // token to launcher.json — no service restart needed (#1237).
        let auth_token = cli_auth_token
            .clone()
            .or_else(|| crate::config::load_config().auth_token);

        info!("Connecting to backend: {}", backend_url);

        match ws_bridge::native_client::connect::<LauncherEndpoint>(backend_url).await {
            Ok(conn) => {
                info!("Connected to backend");
                backoff = Duration::from_secs(1);

                let (mut ws_sender, mut ws_receiver) = conn.split();

                // Send registration
                let register = LauncherToServer::LauncherRegister {
                    protocol_version: Some(shared::protocol::PROTOCOL_VERSION),
                    launcher_id,
                    launcher_name: launcher_name.to_string(),
                    auth_token: auth_token.clone(),
                    hostname: claude_session_lib::hostname_or_unknown(),
                    version: Some(shared::VERSION.to_string()),
                    working_directory: std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string()),
                    capabilities: vec![
                        shared::LAUNCHER_CAPABILITY_SESSION_PLUGINS.to_string(),
                        shared::LAUNCHER_CAPABILITY_PLUGINS.to_string(),
                        shared::LAUNCHER_CAPABILITY_CREATE_WORKTREE.to_string(),
                        shared::LAUNCHER_CAPABILITY_SCRATCH_WORKTREE.to_string(),
                        shared::LAUNCHER_CAPABILITY_FORK_SESSION.to_string(),
                        shared::LAUNCHER_CAPABILITY_IMPORT_CODEX.to_string(),
                        shared::LAUNCHER_CAPABILITY_RESTORE_SESSION.to_string(),
                        shared::LAUNCHER_CAPABILITY_HISTORY_RESTORE.to_string(),
                        shared::LAUNCHER_CAPABILITY_RESTART.to_string(),
                        shared::LAUNCHER_CAPABILITY_HEARTBEAT_ACK.to_string(),
                    ],
                };
                if ws_sender.send(register).await.is_err() {
                    warn!("Failed to send registration");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }

                // Wait for RegisterAck. `Err(reason)` = fatal rejection.
                let mut server_capabilities: Vec<String> = Vec::new();
                let ack_ok = loop {
                    match ws_receiver.recv().await {
                        Some(Ok(ServerToLauncher::LauncherRegisterAck {
                            protocol_version,
                            success,
                            error,
                            fatal,
                            reject_reason,
                            capabilities,
                            ..
                        })) => {
                            if let Err(error) =
                                shared::protocol::check_protocol_version(protocol_version, "server")
                            {
                                error!("{error}");
                                break Err(ParkedReason::ProtocolIncompatible);
                            }
                            if success {
                                info!("Registration successful");
                                server_capabilities = capabilities;
                                break Ok(true);
                            }
                            let msg = error.unwrap_or_default();
                            if fatal {
                                error!("Registration rejected (fatal): {}", msg);
                                break Err(ParkedReason::Server(classify_fatal(
                                    reject_reason,
                                    &msg,
                                )));
                            }
                            error!("Registration failed: {}", msg);
                            break Ok(false);
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            warn!("Decode error during registration: {}", e);
                            continue;
                        }
                        None => break Ok(false),
                    }
                };

                match ack_ok {
                    Err(reason) => {
                        // Fatal rejection — park with a slow retry instead of
                        // exiting. Exiting under systemd's Restart=on-failure
                        // was a 5s crash loop (76k restarts on one expired
                        // token, #1237); parking keeps one warm process that
                        // recovers by itself when the cause clears — the token
                        // re-read above picks up an `agent-portal login`
                        // without a restart.
                        if last_parked_reason != Some(reason) {
                            error!("{}", parked_instruction(reason));
                            last_parked_reason = Some(reason);
                        } else {
                            info!(
                                "Still parked ({:?}); retrying in {:?}",
                                reason, parked_backoff
                            );
                        }
                        tokio::time::sleep(parked_backoff).await;
                        parked_backoff = (parked_backoff * 2).min(PARKED_BACKOFF_MAX);
                        continue;
                    }
                    Ok(false) => {
                        warn!("Registration failed, will retry");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                    Ok(true) => {
                        // Healthy again: reset the parked state so a future
                        // rejection logs its instruction afresh.
                        parked_backoff = PARKED_BACKOFF_START;
                        last_parked_reason = None;
                    }
                }

                // RegisterAck alone does not authorize stale queued work.
                supervisor.reconnect();

                // Background native reads return through this connection-owned
                // queue so history copying never blocks heartbeats or controls.
                let (import_reply_tx, mut import_reply_rx) = mpsc::channel::<LauncherToServer>(4);

                // Main loop
                let mut heartbeat_timer = tokio::time::interval(HEARTBEAT_INTERVAL);
                // Resource readings for the dashboard's services monitor: on
                // every heartbeat, and every few seconds when the backend
                // said it accepts the between-heartbeat frame.
                let mut sysmon = portal_sysmon::SystemMonitor::new();
                let fast_stats = server_capabilities
                    .iter()
                    .any(|c| c == shared::SERVER_CAPABILITY_SYSTEM_STATS);
                if fast_stats {
                    info!(
                        "Backend accepts system stats; reporting every {}s",
                        SYSTEM_STATS_INTERVAL.as_secs()
                    );
                } else {
                    info!("Backend predates system stats; reporting on heartbeats only");
                }
                let mut stats_timer = tokio::time::interval(SYSTEM_STATS_INTERVAL);
                stats_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let start = Instant::now();
                // Half-open control-socket detection (#1366). `ack_seen` stays
                // false against an older backend that doesn't echo heartbeats,
                // which keeps the watchdog disabled (no false reconnects); once
                // an ack proves the backend supports it, a subsequent drought
                // past `HEARTBEAT_ACK_TIMEOUT` forces a reconnect.
                let mut last_ack = Instant::now();
                let mut ack_seen = false;

                loop {
                    for request_id in supervisor.cancelled_launches() {
                        scheduler.clear_pending_launch(&request_id);
                    }
                    let sched_dur = scheduler
                        .next_fire_duration()
                        .unwrap_or(Duration::from_secs(3600));
                    let prompt_dur = scheduler
                        .next_prompt_duration()
                        .unwrap_or(Duration::from_secs(3600));
                    let continuation_dur = scheduler
                        .next_continuation_duration()
                        .unwrap_or(Duration::from_secs(3600));
                    let sched_sleep = tokio::time::sleep(sched_dur);
                    let prompt_sleep = tokio::time::sleep(prompt_dur);
                    let continuation_sleep = tokio::time::sleep(continuation_dur);
                    tokio::pin!(sched_sleep);
                    tokio::pin!(prompt_sleep);
                    tokio::pin!(continuation_sleep);

                    tokio::select! {
                        Some(reply) = import_reply_rx.recv() => {
                            if ws_sender.send(reply).await.is_err() { break; }
                        }
                        result = ws_receiver.recv() => {
                            match result {
                                Some(Ok(msg)) => {
                                    supervisor.observe(&msg);
                                    // A heartbeat echo proves the control socket
                                    // is live end-to-end; record it and arm the
                                    // watchdog. Handled here (not in
                                    // `handle_message`) so the liveness state
                                    // stays local to the loop.
                                    if matches!(msg, ServerToLauncher::LauncherHeartbeatAck) {
                                        last_ack = Instant::now();
                                        ack_seen = true;
                                    } else {
                                        handle_message(
                                        msg,
                                        &mut ws_sender,
                                        &mut process_manager,
                                        &mut scheduler,
                                        &mut login_registry,
                                        &import_reply_tx,
                                        ).await;
                                    }
                                    if !supervisor.flush(async |frame| ws_sender.send(frame).await).await { break; }
                                }
                                Some(Err(e)) => {
                                    warn!("Decode error: {}", e);
                                    continue;
                                }
                                None => {
                                    info!("WebSocket closed by server");
                                    break;
                                }
                            }
                        }

                        _ = heartbeat_timer.tick() => {
                            // Half-open detection: if the backend has been
                            // echoing heartbeats but has now gone silent past
                            // the timeout, our sends are buffering into a dead
                            // TCP connection — reconnect rather than believe the
                            // socket is fine while the hub marks our sessions
                            // disconnected (#1366).
                            if control_socket_half_open(ack_seen, last_ack.elapsed()) {
                                warn!(
                                "No launcher heartbeat ack for {}s — control socket half-open; reconnecting to re-establish sessions",
                                last_ack.elapsed().as_secs()
                                );
                                break;
                            }
                            let hb = LauncherToServer::LauncherHeartbeat {
                                launcher_id,
                                running_sessions: process_manager.running_session_ids(),
                                uptime_secs: start.elapsed().as_secs(),
                                system: Some(sysmon.sample()),
                            };
                            if ws_sender.send(hb).await.is_err() {
                                warn!("Failed to send heartbeat");
                                break;
                            }

                            // Enforce max runtime on scheduled sessions
                            for session_id in scheduler.timed_out_sessions() {
                                process_manager.stop(&session_id).await;
                            }
                        }

                        _ = stats_timer.tick(), if fast_stats => {
                            let frame = LauncherToServer::SystemStats {
                                launcher_id,
                                sample: sysmon.sample(),
                            };
                            if ws_sender.send(frame).await.is_err() {
                                warn!("Failed to send system stats");
                                break;
                            }
                        }

                        Some(exited) = exit_rx.recv() => {
                            supervisor.extend([process_events::exited(&mut process_manager, &mut scheduler, exited)]);
                            if !supervisor.flush(async |frame| ws_sender.send(frame).await).await { break; }
                        }

                        // Scheduler: fire due tasks
                        _ = &mut sched_sleep, if supervisor.schedule_ready() => {
                            supervisor.extend(scheduler_events::due_tasks(&mut scheduler));
                            if !supervisor.flush(async |frame| ws_sender.send(frame).await).await { break; }
                        }

                        // Scheduler: send pending prompts after delay
                        _ = &mut prompt_sleep, if supervisor.schedule_ready() => {
                            supervisor.extend(scheduler_events::due_prompts(&mut scheduler));
                            if !supervisor.flush(async |frame| ws_sender.send(frame).await).await { break; }
                        }

                        _ = &mut continuation_sleep, if supervisor.continuations_ready() => {
                            supervisor.extend(scheduler_events::due_continuations(&mut scheduler, &process_manager.running_session_ids()));
                            if !supervisor.flush(async |frame| ws_sender.send(frame).await).await { break; }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect: {}", e);
            }
        }

        info!("Reconnecting in {:?}...", backoff);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_fatal_prefers_machine_readable_reason() {
        // A backend-supplied reason wins even over contradictory text.
        assert_eq!(
            classify_fatal(
                Some(LauncherRejectReason::DuplicateLauncher),
                "Authentication failed"
            ),
            LauncherRejectReason::DuplicateLauncher
        );
    }

    #[test]
    fn classify_fatal_string_fallback_covers_old_backends() {
        // Pre-reject_reason backends: sniff the message text.
        assert_eq!(
            classify_fatal(None, "Authentication failed"),
            LauncherRejectReason::AuthFailed
        );
        assert_eq!(
            classify_fatal(None, "No auth token provided"),
            LauncherRejectReason::AuthFailed
        );
        assert_eq!(
            classify_fatal(None, "You already have 10 launchers connected (max 10)."),
            LauncherRejectReason::TooManyLaunchers
        );
        // Case-insensitive on every arm, not just the auth sniff.
        assert_eq!(
            classify_fatal(None, "YOU ALREADY HAVE 10 LAUNCHERS CONNECTED."),
            LauncherRejectReason::TooManyLaunchers
        );
        assert_eq!(
            classify_fatal(
                None,
                "A launcher named 'x' is already connected from this host."
            ),
            LauncherRejectReason::DuplicateLauncher
        );
    }

    #[test]
    fn every_park_reason_has_an_instruction() {
        for reason in [
            ParkedReason::ProtocolIncompatible,
            ParkedReason::Server(LauncherRejectReason::AuthFailed),
            ParkedReason::Server(LauncherRejectReason::TooManyLaunchers),
            ParkedReason::Server(LauncherRejectReason::DuplicateLauncher),
        ] {
            assert!(!parked_instruction(reason).is_empty());
        }
        assert!(
            parked_instruction(ParkedReason::Server(LauncherRejectReason::AuthFailed))
                .contains("agent-portal login")
        );
    }

    #[test]
    fn half_open_watchdog_never_fires_without_an_ack() {
        // An older backend never echoes heartbeats, so `ack_seen` stays false
        // and the watchdog must stay disabled no matter how long it's been —
        // otherwise a new launcher would reconnect-loop against a healthy hub.
        assert!(!control_socket_half_open(
            false,
            HEARTBEAT_ACK_TIMEOUT * 100
        ));
    }

    #[test]
    fn half_open_watchdog_fires_only_after_ack_drought() {
        // Seen an ack and within the window: healthy.
        assert!(!control_socket_half_open(true, HEARTBEAT_ACK_TIMEOUT / 2));
        // Seen an ack but silent past the timeout: half-open, reconnect.
        assert!(control_socket_half_open(
            true,
            HEARTBEAT_ACK_TIMEOUT + Duration::from_secs(1)
        ));
    }
}
