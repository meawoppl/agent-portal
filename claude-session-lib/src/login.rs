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

use claude_codes::auth::{LoginFlow, LoginMode};
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
/// Longest failure detail we relay — enough of the CLI transcript to diagnose,
/// bounded so a runaway TUI dump doesn't flood the UI.
const MESSAGE_TAIL: usize = 400;

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
            let _ = url_tx.send(Err(e.to_string()));
            return;
        }
    };
    let url = match flow.auth_url(AUTH_URL_TIMEOUT) {
        Ok(url) => url,
        Err(e) => {
            let _ = url_tx.send(Err(e.to_string()));
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
    let outcome = match result {
        Ok(out) if out.credentials_updated => AgentLoginOutcome {
            done: true,
            success: true,
            message: None,
        },
        Ok(out) => failed(&transcript_tail(&out.transcript)),
        Err(e) => {
            let mut message = e.to_string();
            if matches!(e, claude_codes::Error::CodeRejected { .. }) {
                message.push_str(
                    " — the code may have expired, been used already, or come \
                     from an earlier sign-in window. Start a new sign-in and \
                     paste the fresh code promptly.",
                );
            }
            failed(&message)
        }
    };
    let _ = outcome_tx.send(outcome);
}

fn failed(message: &str) -> AgentLoginOutcome {
    AgentLoginOutcome {
        done: true,
        success: false,
        message: Some(message.to_string()),
    }
}

/// Keep the tail of a CLI transcript for a failure message — the end carries
/// the error, and it's char-safe against multi-byte output.
fn transcript_tail(transcript: &str) -> String {
    let trimmed = transcript.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= MESSAGE_TAIL {
        return trimmed.to_string();
    }
    format!(
        "…{}",
        chars[chars.len() - MESSAGE_TAIL..]
            .iter()
            .collect::<String>()
    )
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

    #[test]
    fn transcript_tail_keeps_the_end_and_is_char_safe() {
        assert_eq!(transcript_tail("  short  "), "short");
        let long = "é".repeat(MESSAGE_TAIL + 50);
        let tail = transcript_tail(&long);
        assert!(tail.starts_with('…'));
        assert_eq!(tail.chars().count(), MESSAGE_TAIL + 1);
    }

    #[test]
    fn failed_outcome_is_settled_and_carries_the_message() {
        let o = failed("nope");
        assert!(o.done && !o.success);
        assert_eq!(o.message.as_deref(), Some("nope"));
    }
}
