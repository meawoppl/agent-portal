//! Interactive claude sign-in for the portal's launcher-driven login surface.
//!
//! Wraps claude-codes' PTY-backed [`LoginFlow`] as a *parkable* session: `start`
//! drives `claude auth login --claudeai` (persisted subscription login) far
//! enough to hand back the OAuth URL, then watches browser approval on its own
//! thread while accepting a pasted fallback code through [`submit_code`].
//!
//! Two rules from the login contract are load-bearing here:
//! - the flow's blocking PTY calls run on a dedicated `std::thread`, never on
//!   the async runtime;
//! - dropping the session disconnects the code channel, so the worker drops the
//!   `LoginFlow`, whose `Drop` SIGTERMs the PTY child ("LoginFlow dropped while
//!   unfinished" in the logs) — a closed browser tab reaps cleanly.
//!
//! A rejected code settles the flow as a failure (the caller restarts with a
//! fresh session) rather than driving `retry_new_url` in place — "drop and
//! restart" is the contract-sanctioned handling, and the error screen has no
//! input field for a same-URL retry anyway.
//!
//! [`submit_code`]: ClaudeLoginSession::submit_code

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use claude_codes::auth::{AuthStatus, LoginFlow, LoginMode, LoginOutcome};
use shared::{AgentLoginOutcome, LoginInteraction, LoginPresentable};

/// Wait for the CLI to emit its OAuth URL after start.
const AUTH_URL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the flow waits for browser approval or a fallback code.
const LOGIN_WAIT: Duration = Duration::from_secs(300);
/// Allow the SDK's three-second credential/token grace period to finish even
/// when the subscription login persists credentials without emitting a token.
const APPROVAL_POLL_WAIT: Duration = Duration::from_secs(4);
/// Wait for the CLI to settle after a code submission.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound the final status check so a stuck CLI cannot strand the login worker.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// A parked claude login flow, driven on a worker thread.
pub struct ClaudeLoginSession {
    /// Send the pasted code once. Dropping this (session drop) disconnects the
    /// worker's recv, which drops the `LoginFlow` and reaps its PTY child.
    code_tx: Sender<String>,
    /// Cache completion so a browser poll and a fallback submission see the
    /// same result even if approval lands while the user is pasting a code.
    outcome: Mutex<LoginOutcomeState>,
}

struct LoginOutcomeState {
    rx: Receiver<AgentLoginOutcome>,
    settled: Option<AgentLoginOutcome>,
}

impl ClaudeLoginSession {
    /// Start the flow and return the URL for the user to open. Blocks only
    /// until the CLI prints its URL (bounded by [`AUTH_URL_TIMEOUT`]).
    ///
    /// Runs on the caller's thread up to that point; call it from a
    /// `spawn_blocking` context on the launcher.
    pub fn start() -> Result<(Self, LoginPresentable, LoginInteraction), String> {
        let (code_tx, code_rx) = std::sync::mpsc::channel::<String>();
        let (url_tx, url_rx) = std::sync::mpsc::channel::<Result<String, String>>();
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel::<AgentLoginOutcome>();

        std::thread::spawn(move || run_flow(code_rx, &url_tx, &outcome_tx));

        match url_rx.recv_timeout(AUTH_URL_TIMEOUT + Duration::from_secs(5)) {
            Ok(Ok(url)) => Ok((
                Self {
                    code_tx,
                    outcome: Mutex::new(LoginOutcomeState {
                        rx: outcome_rx,
                        settled: None,
                    }),
                },
                LoginPresentable::AuthUrl { url },
                LoginInteraction::SubmitCode,
            )),
            Ok(Err(e)) => Err(e),
            // Worker died or hung before yielding a URL.
            Err(_) => Err("claude did not produce a sign-in URL in time".to_string()),
        }
    }

    /// Feed the pasted code and wait for the flow to settle.
    ///
    /// Blocks until the CLI confirms or rejects (bounded by [`SUBMIT_TIMEOUT`]);
    /// call it from a `spawn_blocking` context.
    pub fn submit_code(&self, code: String) -> AgentLoginOutcome {
        let outcome = self.poll();
        if outcome.done {
            return outcome;
        }
        if self.code_tx.send(code).is_err() {
            return self.poll();
        }
        let deadline =
            Instant::now() + SUBMIT_TIMEOUT + APPROVAL_POLL_WAIT + Duration::from_secs(5);
        loop {
            let outcome = self.poll();
            if outcome.done {
                return outcome;
            }
            if Instant::now() >= deadline {
                return failed("timed out waiting for the sign-in to complete");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Observe either browser approval or code completion without blocking the
    /// launcher's async runtime. Keep the terminal result for concurrent callers.
    pub fn poll(&self) -> AgentLoginOutcome {
        let mut state = self.outcome.lock().unwrap();
        if state.settled.is_none() {
            state.settled = match state.rx.try_recv() {
                Ok(outcome) => Some(outcome),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(failed("the sign-in worker ended without an outcome"))
                }
            };
        }
        state.settled.clone().unwrap_or(AgentLoginOutcome {
            done: false,
            success: false,
            message: None,
        })
    }
}

fn run_flow(
    code_rx: Receiver<String>,
    url_tx: &Sender<Result<String, String>>,
    outcome_tx: &Sender<AgentLoginOutcome>,
) {
    let mut flow = match LoginFlow::start(LoginMode::ClaudeAi) {
        Ok(flow) => flow,
        Err(e) => {
            let _ = url_tx.send(Err(login_error_message(&e).to_owned()));
            return;
        }
    };
    let url = match flow.auth_url(AUTH_URL_TIMEOUT) {
        Ok(url) => url,
        Err(e) => {
            let _ = url_tx.send(Err(login_error_message(&e).to_owned()));
            return;
        }
    };
    // If the receiver is gone the session was dropped between spawn and now;
    // return, dropping `flow` → PTY child reaped.
    if url_tx.send(Ok(url)).is_err() {
        return;
    }

    let deadline = Instant::now() + LOGIN_WAIT;
    let result = loop {
        match code_rx.try_recv() {
            Ok(code) => break flow.submit_code_and_wait(&code, SUBMIT_TIMEOUT),
            // Cancel drops the flow and reaps the PTY child.
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            let _ = outcome_tx.send(failed("timed out waiting for sign-in approval or a code"));
            return;
        }
        // Browser approval normally writes credentials without a pasted code.
        // A full grace window is needed by poll_outcome for tokenless logins.
        match flow.poll_outcome(APPROVAL_POLL_WAIT) {
            Ok(Some(outcome)) => break Ok(outcome),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
    };
    let _ = outcome_tx.send(login_outcome(result, verified_status));
}

/// Clean exit is not proof of login, and success-looking transcript text is
/// never authentication evidence. File-only SDK detection can miss credentials
/// stored elsewhere; confirm the CLI's typed subscription status instead.
/// Upstream tracking: rust-code-agent-sdks#559.
fn login_outcome(
    result: claude_codes::Result<LoginOutcome>,
    verify: impl FnOnce() -> Option<AuthStatus>,
) -> AgentLoginOutcome {
    match result {
        Ok(out) if out.credentials_updated => succeeded(),
        Ok(_) | Err(claude_codes::Error::LoginChildExited { code: Some(0), .. }) => {
            match verify() {
                Some(status) if status.logged_in && status.auth_method.as_deref() == Some("claude.ai") => succeeded(),
                Some(status) if !status.logged_in => failed(
                    "Claude finished, but still reports that it is signed out. Start a new sign-in.",
                ),
                _ => failed(
                    "Claude finished, but its subscription sign-in could not be verified. Check Claude's sign-in status on this computer, then try again.",
                ),
            }
        }
        Err(error) => failed(login_error_message(&error)),
    }
}

fn succeeded() -> AgentLoginOutcome {
    AgentLoginOutcome {
        done: true,
        success: true,
        message: None,
    }
}

/// Use the SDK's typed response, with a process deadline and kill-on-drop.
/// SDK auth_status is synchronous and has no timeout. Never relay stdout,
/// stderr or parser errors: login output can contain OAuth URLs and tokens.
fn verified_status() -> Option<AuthStatus> {
    verified_status_with_binary(std::ffi::OsStr::new("claude"), STATUS_TIMEOUT)
}

fn verified_status_with_binary(binary: &std::ffi::OsStr, timeout: Duration) -> Option<AuthStatus> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let output = tokio::time::timeout(
            timeout,
            tokio::process::Command::new(binary)
                .args(["auth", "status", "--json"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        // Logged-out status is valid JSON even when the command exits nonzero.
        let status: AuthStatus = serde_json::from_slice(&output.stdout).ok()?;
        if status.logged_in && !output.status.success() {
            return None;
        }
        Some(status)
    })
}

/// SDK errors retain diagnostic transcripts. Only these fixed, actionable
/// messages may cross the launcher boundary into the UI or server logs.
fn login_error_message(error: &claude_codes::Error) -> &'static str {
    match error {
        claude_codes::Error::CodeRejected { .. } => {
            "Claude rejected the sign-in code. Start a new sign-in and use the new code."
        }
        claude_codes::Error::LoginTimeout { .. } | claude_codes::Error::Timeout => {
            "Timed out waiting for Claude sign-in. Start a new sign-in and try again."
        }
        claude_codes::Error::LoginChildExited { .. } => {
            "Claude exited before sign-in could be confirmed. Start a new sign-in and try again."
        }
        claude_codes::Error::BinaryNotFound { .. } => {
            "Claude is not available on this computer. Install it before signing in."
        }
        _ => "Could not complete Claude sign-in. Try again on this computer.",
    }
}

fn failed(message: &str) -> AgentLoginOutcome {
    AgentLoginOutcome {
        done: true,
        success: false,
        message: Some(message.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (
        ClaudeLoginSession,
        Sender<AgentLoginOutcome>,
        Receiver<String>,
    ) {
        let (code_tx, code_rx) = std::sync::mpsc::channel();
        let (outcome_tx, rx) = std::sync::mpsc::channel();
        (
            ClaudeLoginSession {
                code_tx,
                outcome: Mutex::new(LoginOutcomeState { rx, settled: None }),
            },
            outcome_tx,
            code_rx,
        )
    }

    #[test]
    fn browser_approval_completes_without_a_code_and_is_cached() {
        let (session, tx, code_rx) = session();
        assert!(!session.poll().done);
        let success = AgentLoginOutcome {
            done: true,
            success: true,
            message: None,
        };
        tx.send(success.clone()).unwrap();
        drop(tx);
        assert_eq!(session.poll(), success);
        assert_eq!(session.poll(), success);
        assert_eq!(session.submit_code("late fallback".into()), success);
        assert!(matches!(code_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn fallback_submission_and_poll_share_the_result() {
        let (session, tx, code_rx) = session();
        let worker = std::thread::spawn(move || {
            assert_eq!(code_rx.recv().unwrap(), "fallback");
            tx.send(failed("rejected code")).unwrap();
        });
        assert_eq!(
            session.submit_code("fallback".into()),
            failed("rejected code")
        );
        assert_eq!(session.poll(), failed("rejected code"));
        worker.join().unwrap();
    }

    #[test]
    fn worker_exit_is_terminal_instead_of_pending_forever() {
        let (session, tx, _code_rx) = session();
        drop(tx);
        let outcome = session.poll();
        assert!(outcome.done && !outcome.success);
        assert_eq!(session.poll(), outcome);
    }

    #[test]
    fn dropping_session_disconnects_code_receiver() {
        let (session, _tx, code_rx) = session();
        drop(session);
        assert!(matches!(
            code_rx.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    fn status(logged_in: bool, method: &str) -> AuthStatus {
        AuthStatus {
            logged_in,
            auth_method: Some(method.into()),
            api_provider: None,
            email: None,
            org_id: None,
            org_name: None,
            subscription_type: None,
            extra: Default::default(),
        }
    }

    fn exited(code: u32) -> claude_codes::Result<LoginOutcome> {
        Err(claude_codes::Error::LoginChildExited {
            code: Some(code),
            transcript: "https://example.invalid/oauth?code=SENSITIVE Login successful.".into(),
        })
    }

    #[test]
    fn clean_exit_requires_verified_subscription_status() {
        assert!(login_outcome(exited(0), || Some(status(true, "claude.ai"))).success);
        for verified in [
            None,
            Some(status(false, "claude.ai")),
            Some(status(true, "console")),
            Some(status(true, "api_key")),
        ] {
            let outcome = login_outcome(exited(0), || verified);
            assert!(outcome.done && !outcome.success);
            let message = outcome.message.unwrap();
            assert!(!message.contains("SENSITIVE"));
            assert!(!message.contains("Login successful"));
        }
    }

    #[test]
    fn failed_exit_does_not_accept_preexisting_login_or_success_text() {
        let outcome = login_outcome(exited(1), || {
            panic!("must not query existing credentials after failure")
        });
        assert!(outcome.done && !outcome.success);
        assert!(!outcome.message.unwrap().contains("SENSITIVE"));
    }

    #[test]
    fn sdk_failures_never_relay_authentication_transcripts() {
        for error in [
            claude_codes::Error::LoginTimeout {
                transcript: "secret-url".into(),
            },
            claude_codes::Error::CodeRejected {
                message: "secret-code".into(),
            },
            claude_codes::Error::Unknown("secret-token".into()),
            claude_codes::Error::BinaryNotFound {
                name: "secret-path".into(),
            },
        ] {
            let message = login_error_message(&error);
            assert!(!message.contains("secret-"));
            assert!(!message.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn status_check_uses_typed_json_and_bounds_the_child_lifetime() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("claude-fixture");
        for (body, expected) in [
            (
                r#"printf '%s' '{"loggedIn":true,"authMethod":"claude.ai"}'"#,
                Some(true),
            ),
            (r#"printf '%s' '{"loggedIn":false}'; exit 1"#, Some(false)),
            (
                r#"printf '%s' '{"loggedIn":true,"authMethod":"claude.ai"}'; exit 1"#,
                None,
            ),
            ("printf 'Login successful. secret-url'", None),
            ("exec sleep 30", None),
        ] {
            std::fs::write(&binary, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            let start = Instant::now();
            let status =
                verified_status_with_binary(binary.as_os_str(), Duration::from_millis(100));
            assert_eq!(status.map(|s| s.logged_in), expected);
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn observed_credential_update_keeps_the_existing_success_path() {
        let outcome = LoginOutcome {
            token: None,
            token_source: None,
            credentials_updated: true,
            osc52: claude_codes::auth::Osc52Status::Absent,
            copy_nudge_sent: false,
            transcript: "secret-url".into(),
        };
        assert_eq!(
            login_outcome(Ok(outcome), || panic!("already confirmed")),
            succeeded()
        );
    }

    #[test]
    fn failed_outcome_is_settled_and_carries_the_message() {
        let o = failed("nope");
        assert!(o.done && !o.success);
        assert_eq!(o.message.as_deref(), Some("nope"));
    }
}
