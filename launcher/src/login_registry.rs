//! Launcher-side registry of in-flight interactive agent logins.
//!
//! A login spans several backend RPCs — start (present a URL/code), then either
//! a code submission (claude) or polling for browser completion (codex) — so
//! the flow must live *between* messages, keyed by a `flow_id` the backend
//! mints. This holds those parked sessions and adapts each agent's mechanics to
//! one interface:
//! - claude's calls are blocking (PTY), so they run on `spawn_blocking`;
//! - codex's are async (app-server connection).
//!
//! Dropping a session (cancel, or the whole registry on disconnect) tears down
//! its child — see each driver's `Drop`.

use std::collections::HashMap;
use std::sync::Arc;

use shared::{AgentLoginOutcome, AgentType, LoginInteraction, LoginPresentable};
use uuid::Uuid;

use claude_session_lib::login::ClaudeLoginSession;
use codex_session_lib::login::CodexLoginSession;

/// One parked login flow, per agent.
enum LoginSession {
    Claude(ClaudeLoginSession),
    Codex(CodexLoginSession),
}

impl LoginSession {
    fn submit_code(&self, code: String) -> AgentLoginOutcome {
        match self {
            Self::Claude(s) => s.submit_code(code),
            Self::Codex(s) => s.submit_code(code),
        }
    }

    fn poll(&self) -> AgentLoginOutcome {
        match self {
            Self::Claude(s) => s.poll(),
            Self::Codex(s) => s.poll(),
        }
    }
}

/// Parked login flows, keyed by the backend-minted `flow_id`.
#[derive(Default)]
pub struct LoginRegistry {
    flows: HashMap<Uuid, Arc<LoginSession>>,
}

impl LoginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a login for `agent_type`, park it under `flow_id`, and return what
    /// the user must act on. `Err` if the flow couldn't even begin.
    pub async fn start(
        &mut self,
        flow_id: Uuid,
        agent_type: AgentType,
    ) -> Result<(LoginPresentable, LoginInteraction), String> {
        let (session, presentable, interaction) = match agent_type {
            AgentType::Claude => {
                // `start` blocks until the CLI prints its URL — off-runtime.
                let started = tokio::task::spawn_blocking(ClaudeLoginSession::start)
                    .await
                    .map_err(|e| format!("claude login task failed to run: {e}"))??;
                (LoginSession::Claude(started.0), started.1, started.2)
            }
            AgentType::Codex => {
                let (s, p, i) = CodexLoginSession::start().await?;
                (LoginSession::Codex(s), p, i)
            }
        };
        self.flows.insert(flow_id, Arc::new(session));
        Ok((presentable, interaction))
    }

    /// Feed a pasted code to a parked flow and wait for it to settle.
    pub async fn submit_code(&self, flow_id: Uuid, code: String) -> AgentLoginOutcome {
        let Some(session) = self.flows.get(&flow_id).cloned() else {
            return gone();
        };
        // claude's submit blocks; codex's is a trivial message. Run both under
        // spawn_blocking for uniformity (codex's is instant).
        tokio::task::spawn_blocking(move || session.submit_code(code))
            .await
            .unwrap_or_else(|e| AgentLoginOutcome {
                done: true,
                success: false,
                message: Some(format!("login task failed: {e}")),
            })
    }

    /// Current state of a parked flow (for the poll path).
    pub fn poll(&self, flow_id: Uuid) -> AgentLoginOutcome {
        match self.flows.get(&flow_id) {
            Some(session) => session.poll(),
            None => gone(),
        }
    }

    /// Drop a flow, tearing down its child. Called on cancel and after a
    /// settled outcome so parked PTYs / app-servers don't linger.
    pub fn remove(&mut self, flow_id: Uuid) {
        self.flows.remove(&flow_id);
    }
}

fn gone() -> AgentLoginOutcome {
    AgentLoginOutcome {
        done: true,
        success: false,
        message: Some("that sign-in session is no longer active — start again".into()),
    }
}
