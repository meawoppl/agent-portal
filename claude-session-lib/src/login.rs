//! Interactive claude sign-in for the portal's launcher-driven login surface.
//!
//! Wraps claude-codes' PTY-backed [`LoginFlow`] as a *parkable* session: `start`
//! drives `claude auth login --claudeai` (persisted subscription login) far
//! enough to hand back the OAuth URL, then the flow waits — on its own thread —
//! for the code the user pastes back from the browser, which [`submit_code`]
//! feeds in.
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

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::Duration;

use claude_codes::auth::{LoginFlow, LoginMode};
use shared::{AgentLoginOutcome, LoginInteraction, LoginPresentable};

/// Wait for the CLI to emit its OAuth URL after start.
const AUTH_URL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the parked flow waits for the user to paste a code back.
const CODE_WAIT: Duration = Duration::from_secs(300);
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
    /// The settled outcome, delivered once by the worker. Behind a `Mutex<Option<>>`
    /// so `submit_code` can consume it exactly once.
    outcome_rx: Mutex<Option<Receiver<AgentLoginOutcome>>>,
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
                    outcome_rx: Mutex::new(Some(outcome_rx)),
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
        if self.code_tx.send(code).is_err() {
            return failed("the sign-in session ended before the code was submitted");
        }
        let rx = self.outcome_rx.lock().unwrap().take();
        match rx {
            Some(rx) => match rx.recv_timeout(SUBMIT_TIMEOUT + Duration::from_secs(5)) {
                Ok(outcome) => outcome,
                Err(_) => failed("timed out waiting for the sign-in to complete"),
            },
            // A second submit against an already-consumed session.
            None => failed("the code was already submitted for this session"),
        }
    }

    /// Claude has no in-browser completion — it settles only via
    /// [`submit_code`](Self::submit_code) — so a poll before then is "pending".
    pub fn poll(&self) -> AgentLoginOutcome {
        AgentLoginOutcome {
            done: false,
            success: false,
            message: None,
        }
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

    let outcome = match code_rx.recv_timeout(CODE_WAIT) {
        Ok(code) => match flow.submit_code_and_wait(&code, SUBMIT_TIMEOUT) {
            // `credentials_updated` is the authoritative success signal
            // (the creds store was written), independent of screen scraping.
            Ok(out) if out.credentials_updated => AgentLoginOutcome {
                done: true,
                success: true,
                message: None,
            },
            Ok(out) => AgentLoginOutcome {
                done: true,
                success: false,
                message: Some(transcript_tail(&out.transcript)),
            },
            // CodeRejected / LoginTimeout / LoginChildExited all Display as
            // self-describing text — relay it verbatim (login contract). A
            // rejected code additionally gets recovery guidance: codes are
            // single-use, expire within minutes, and are PKCE-bound to the
            // sign-in window that minted their URL, so the fix is always a
            // fresh sign-in — never re-pasting the old code.
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
        },
        // Disconnected = session dropped (cancel); Timeout = user wandered off.
        // Either way `flow` drops on return → PTY child reaped. Nobody is
        // listening on a cancel, so the send is best-effort.
        Err(RecvTimeoutError::Disconnected) => return,
        Err(RecvTimeoutError::Timeout) => failed("timed out waiting for the sign-in code"),
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
