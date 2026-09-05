//! Interactive codex sign-in for the portal's launcher-driven login surface.
//!
//! Codex is protocol-native: `start` spins a short-lived `codex app-server`,
//! calls `account/login/start` in **device-code** mode, and hands back the
//! `{user_code, verification_url}` for the user to approve in a browser. Unlike
//! claude there is no code to paste back — the flow completes in the browser
//! and the app-server emits an `account/login/completed` notification, which a
//! background task waits for. The caller polls [`poll`] for the outcome.
//!
//! Dropping the session (browser closed) fires the cancel signal: the watcher
//! calls `account/login/cancel` and drops the client, tearing down the
//! app-server child (login contract: cancel reaps).
//!
//! [`poll`]: CodexLoginSession::poll

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use codex_codes::{
    AppServerBuilder, AsyncClient as CodexAsyncClient, CancelLoginAccountParams,
    LoginAccountParams, LoginAccountResponse, Notification, ServerMessage,
};
use shared::{AgentLoginOutcome, LoginInteraction, LoginPresentable};
use tokio::sync::Notify;

/// How long to wait for the user to approve the device code before giving up.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(300);

/// An in-flight codex device-code login, watched on a background task.
pub struct CodexLoginSession {
    outcome: Arc<Mutex<Option<AgentLoginOutcome>>>,
    /// Fired on drop to cancel the login and reap the app-server.
    cancel: Arc<Notify>,
}

impl CodexLoginSession {
    /// Start the app-server, kick off device-code login, and return the code +
    /// URL to present. The completion watcher runs in the background.
    pub async fn start() -> Result<(Self, LoginPresentable, LoginInteraction), String> {
        let mut client = CodexAsyncClient::start_with(AppServerBuilder::new())
            .await
            .map_err(|e| format!("could not start the codex app-server: {e}"))?;

        let resp = client
            .account_login_start(&LoginAccountParams::ChatgptDeviceCode)
            .await
            .map_err(|e| format!("codex sign-in could not start: {e}"))?;

        let (user_code, verification_url, login_id) = match resp {
            LoginAccountResponse::ChatgptDeviceCode {
                user_code,
                verification_url,
                login_id,
            } => (user_code, verification_url, login_id),
            other => {
                return Err(format!(
                    "codex returned an unexpected sign-in mode: {other:?}"
                ))
            }
        };

        let outcome = Arc::new(Mutex::new(None));
        let cancel = Arc::new(Notify::new());
        tokio::spawn(watch(client, login_id, outcome.clone(), cancel.clone()));

        Ok((
            Self { outcome, cancel },
            LoginPresentable::DeviceCode {
                user_code,
                verification_url,
            },
            LoginInteraction::AwaitCompletion,
        ))
    }

    /// Current outcome; `done == false` until the browser approval lands.
    ///
    /// A poisoned mutex still yields its guarded state — poisoning is sticky,
    /// so discarding the guard would wedge the login as pending forever.
    pub fn poll(&self) -> AgentLoginOutcome {
        self.outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or(AgentLoginOutcome {
                done: false,
                success: false,
                message: None,
            })
    }

    /// Codex needs no pasted code — this only exists for a uniform registry
    /// interface and reports that.
    pub fn submit_code(&self, _code: String) -> AgentLoginOutcome {
        AgentLoginOutcome {
            done: true,
            success: false,
            message: Some("codex sign-in completes in the browser — no code to enter here".into()),
        }
    }
}

impl Drop for CodexLoginSession {
    fn drop(&mut self) {
        // Wake the watcher so it cancels the login and drops the client
        // (app-server reaped). If the watcher already finished this is a no-op.
        self.cancel.notify_waiters();
    }
}

async fn watch(
    mut client: CodexAsyncClient,
    login_id: String,
    outcome: Arc<Mutex<Option<AgentLoginOutcome>>>,
    cancel: Arc<Notify>,
) {
    // Recover the guard on poisoning (sticky): dropping the update would
    // wedge the login, since no later poll could ever observe it.
    let set = |o: AgentLoginOutcome| {
        *outcome.lock().unwrap_or_else(PoisonError::into_inner) = Some(o);
    };
    let deadline = tokio::time::sleep(DEVICE_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            // Distinct borrows: `cancel`/`deadline` touch neither `client`, so
            // this compiles despite `next_message` holding `&mut client`.
            _ = cancel.notified() => {
                let _ = client
                    .account_login_cancel(&CancelLoginAccountParams { login_id: login_id.clone() })
                    .await;
                return;
            }
            _ = &mut deadline => {
                let _ = client
                    .account_login_cancel(&CancelLoginAccountParams { login_id: login_id.clone() })
                    .await;
                set(failed("codex sign-in timed out waiting for approval"));
                return;
            }
            msg = client.next_message() => match msg {
                Ok(Some(ServerMessage::Notification(Notification::AccountLoginCompleted(n)))) => {
                    set(AgentLoginOutcome {
                        done: true,
                        success: n.success,
                        // Relay the CLI's own error text on failure.
                        message: n.error,
                    });
                    return;
                }
                // Any other server traffic during login is not ours to handle.
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => {
                    set(failed("the codex app-server closed during sign-in"));
                    return;
                }
            }
        }
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

    /// Poisoning is sticky: `poll` must recover the guarded state instead of
    /// discarding it, or the login wedges as pending forever.
    #[test]
    fn poll_recovers_poisoned_outcome() {
        let session = CodexLoginSession {
            outcome: Arc::new(Mutex::new(Some(AgentLoginOutcome {
                done: true,
                success: true,
                message: None,
            }))),
            cancel: Arc::new(Notify::new()),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = session.outcome.lock().unwrap();
            panic!("poison probe");
        }));
        let outcome = session.poll();
        assert!(outcome.done && outcome.success);
    }
}
