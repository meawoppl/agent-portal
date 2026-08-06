//! Interactive agent sign-in modal (Settings ▸ Agents).
//!
//! Drives one launcher-side login flow to completion over the four backend
//! relay endpoints (`/api/launchers/{id}/agent-login/...`). The two agents take
//! different shapes, expressed by [`LoginInteraction`]:
//!
//! - **claude** ([`LoginInteraction::SubmitCode`]): show the auth URL, let the
//!   user paste the code the browser hands back, POST it, await the outcome.
//! - **codex** ([`LoginInteraction::AwaitCompletion`]): show the device code +
//!   verification URL, then poll until the browser approval lands.
//!
//! Closing the modal before the flow settles cancels it launcher-side
//! (fire-and-forget), so a parked PTY / app-server never lingers. A successful
//! sign-in fires `on_success` so the matrix can re-probe.

use gloo_net::http::Request;
use shared::api::{StartAgentLoginRequest, StartAgentLoginResponse, SubmitAgentLoginCodeRequest};
use shared::{AgentLoginOutcome, AgentType, LoginInteraction, LoginPresentable};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::utils;

/// How often to poll a browser-completion (codex) flow, in milliseconds.
const POLL_INTERVAL_MS: u32 = 2000;

#[derive(Properties, PartialEq)]
pub struct AgentLoginModalProps {
    pub launcher_id: Uuid,
    pub agent_type: AgentType,
    /// Human label for the agent (e.g. "Claude"), for the modal title.
    pub agent_name: String,
    /// Fired when the modal should close (cancel, or after a settled flow).
    pub on_close: Callback<()>,
    /// Fired once when a sign-in succeeds, so the caller can re-probe.
    pub on_success: Callback<()>,
}

/// Where the flow currently is. The component owns `flow_id` separately (it's
/// needed for cancel-on-close regardless of stage).
enum Stage {
    /// `POST /start` in flight.
    Starting,
    /// Flow started; waiting on the user. For claude this is the paste-a-code
    /// step; for codex it's the poll-until-approved wait.
    Presenting {
        presentable: LoginPresentable,
        interaction: LoginInteraction,
    },
    /// Terminal: the flow settled (success or a reported failure).
    Settled(AgentLoginOutcome),
    /// Terminal: we couldn't run the flow at all (start failed, transport).
    Failed(String),
}

pub enum Msg {
    Started(Result<StartAgentLoginResponse, String>),
    CodeInput(String),
    SubmitCode,
    Settled(AgentLoginOutcome),
    /// Timer tick: poll the browser-completion flow once.
    Poll,
    Polled(Result<AgentLoginOutcome, String>),
    Close,
}

pub struct AgentLoginModal {
    stage: Stage,
    flow_id: Option<Uuid>,
    code_input: String,
    /// True while a `POST /code` is in flight, to disable the submit button.
    submitting: bool,
    /// Set once `on_success` has fired, so we never fire it twice.
    notified_success: bool,
}

impl Component for AgentLoginModal {
    type Message = Msg;
    type Properties = AgentLoginModalProps;

    fn create(ctx: &Context<Self>) -> Self {
        let launcher_id = ctx.props().launcher_id;
        let agent_type = ctx.props().agent_type;
        ctx.link()
            .send_future(async move { Msg::Started(start_login(launcher_id, agent_type).await) });
        Self {
            stage: Stage::Starting,
            flow_id: None,
            code_input: String::new(),
            submitting: false,
            notified_success: false,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Started(Ok(resp)) => {
                self.flow_id = Some(resp.flow_id);
                // Codex completes in the browser — start polling immediately.
                if resp.interaction == LoginInteraction::AwaitCompletion {
                    self.schedule_poll(ctx);
                }
                self.stage = Stage::Presenting {
                    presentable: resp.presentable,
                    interaction: resp.interaction,
                };
                true
            }
            Msg::Started(Err(e)) => {
                self.stage = Stage::Failed(e);
                true
            }
            Msg::CodeInput(v) => {
                self.code_input = v;
                true
            }
            Msg::SubmitCode => {
                let (Some(flow_id), false) = (self.flow_id, self.submitting) else {
                    return false;
                };
                let code = self.code_input.trim().to_string();
                if code.is_empty() {
                    return false;
                }
                self.submitting = true;
                let launcher_id = ctx.props().launcher_id;
                ctx.link().send_future(async move {
                    match submit_code(launcher_id, flow_id, code).await {
                        Ok(outcome) => Msg::Settled(outcome),
                        Err(e) => Msg::Settled(AgentLoginOutcome {
                            done: true,
                            success: false,
                            message: Some(e),
                        }),
                    }
                });
                true
            }
            Msg::Poll => {
                let Some(flow_id) = self.flow_id else {
                    return false;
                };
                let launcher_id = ctx.props().launcher_id;
                ctx.link().send_future(async move {
                    Msg::Polled(poll_login(launcher_id, flow_id).await)
                });
                false
            }
            Msg::Polled(Ok(outcome)) => {
                if outcome.done {
                    self.settle(ctx, outcome);
                    true
                } else {
                    // Still waiting on the browser — poll again.
                    self.schedule_poll(ctx);
                    false
                }
            }
            // A transient poll failure (launcher blip) shouldn't kill the flow;
            // keep polling until it settles or the user closes.
            Msg::Polled(Err(_)) => {
                self.schedule_poll(ctx);
                false
            }
            Msg::Settled(outcome) => {
                self.submitting = false;
                self.settle(ctx, outcome);
                true
            }
            Msg::Close => {
                self.cancel_if_unsettled(ctx);
                ctx.props().on_close.emit(());
                false
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let on_close = ctx.link().callback(|_| Msg::Close);
        let on_overlay = ctx.link().callback(|_| Msg::Close);
        let stop = Callback::from(|e: MouseEvent| e.stop_propagation());

        html! {
            <div class="agent-login-overlay" onclick={on_overlay}>
                <div class="agent-login-modal" onclick={stop}>
                    <div class="agent-login-header">
                        <h2>{ format!("Sign in to {}", ctx.props().agent_name) }</h2>
                        <button class="agent-login-close" onclick={on_close.clone()}>{ "×" }</button>
                    </div>
                    <div class="agent-login-body">
                        { self.view_stage(ctx) }
                    </div>
                    <div class="agent-login-footer">
                        <button class="link-button" onclick={on_close}>{ self.close_label() }</button>
                    </div>
                </div>
            </div>
        }
    }
}

impl AgentLoginModal {
    /// Schedule one poll tick `POLL_INTERVAL_MS` from now.
    fn schedule_poll(&self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        spawn_local(async move {
            gloo::timers::future::TimeoutFuture::new(POLL_INTERVAL_MS).await;
            link.send_message(Msg::Poll);
        });
    }

    /// Move to a settled outcome, firing `on_success` exactly once on success.
    fn settle(&mut self, ctx: &Context<Self>, outcome: AgentLoginOutcome) {
        if outcome.success && !self.notified_success {
            self.notified_success = true;
            ctx.props().on_success.emit(());
        }
        self.stage = Stage::Settled(outcome);
    }

    /// Cancel the launcher-side flow if it's still parked (browser closed
    /// mid-flow). No-op once settled — the launcher already dropped it.
    fn cancel_if_unsettled(&self, ctx: &Context<Self>) {
        if matches!(self.stage, Stage::Settled(_) | Stage::Failed(_)) {
            return;
        }
        if let Some(flow_id) = self.flow_id {
            cancel_login(ctx.props().launcher_id, flow_id);
        }
    }

    fn close_label(&self) -> &'static str {
        match self.stage {
            Stage::Settled(_) | Stage::Failed(_) => "Close",
            _ => "Cancel",
        }
    }

    fn view_stage(&self, ctx: &Context<Self>) -> Html {
        match &self.stage {
            Stage::Starting => html! { <p class="agent-login-status">{ "Starting sign-in…" }</p> },
            Stage::Failed(msg) => html! {
                <p class="agent-login-status error">{ msg }</p>
            },
            Stage::Settled(outcome) => {
                let (class, text) = outcome_status(outcome);
                html! { <p class={classes!("agent-login-status", class)}>{ text }</p> }
            }
            Stage::Presenting {
                presentable,
                interaction,
            } => self.view_presenting(ctx, presentable, *interaction),
        }
    }

    fn view_presenting(
        &self,
        ctx: &Context<Self>,
        presentable: &LoginPresentable,
        interaction: LoginInteraction,
    ) -> Html {
        match presentable {
            LoginPresentable::AuthUrl { url } => {
                let on_input = ctx.link().callback(|e: InputEvent| {
                    let input: HtmlInputElement = e.target_unchecked_into();
                    Msg::CodeInput(input.value())
                });
                let on_submit = ctx.link().callback(|_| Msg::SubmitCode);
                let on_key = ctx.link().batch_callback(|e: KeyboardEvent| {
                    (e.key() == "Enter").then_some(Msg::SubmitCode)
                });
                html! {
                    <>
                        <p class="agent-login-instr">
                            { "Open this URL, approve the sign-in, then paste the code it gives you:" }
                        </p>
                        <a class="agent-login-url" href={url.clone()} target="_blank" rel="noopener">
                            { url }
                        </a>
                        <div class="agent-login-code-row">
                            <input
                                type="text"
                                class="agent-login-code-input"
                                placeholder="Paste code here"
                                value={self.code_input.clone()}
                                oninput={on_input}
                                onkeypress={on_key}
                                disabled={self.submitting}
                            />
                            <button
                                class="agent-login-submit"
                                onclick={on_submit}
                                disabled={self.submitting || self.code_input.trim().is_empty()}
                            >
                                { if self.submitting { "Signing in…" } else { "Submit" } }
                            </button>
                        </div>
                        // interaction is always SubmitCode here, but keep the shape honest.
                        if interaction != LoginInteraction::SubmitCode {
                            <p class="agent-login-status error">
                                { "Unexpected sign-in mode for this agent." }
                            </p>
                        }
                    </>
                }
            }
            LoginPresentable::DeviceCode {
                user_code,
                verification_url,
            } => html! {
                <>
                    <p class="agent-login-instr">
                        { "Open this URL and enter the code to approve the sign-in:" }
                    </p>
                    <a
                        class="agent-login-url"
                        href={verification_url.clone()}
                        target="_blank"
                        rel="noopener"
                    >
                        { verification_url }
                    </a>
                    <div class="agent-login-device-code">{ user_code }</div>
                    <p class="agent-login-status">{ "Waiting for approval in your browser…" }</p>
                </>
            },
        }
    }
}

/// Cell text + CSS modifier for a settled outcome. Pure, so the success/failure
/// wording is unit-tested without mounting the component.
fn outcome_status(outcome: &AgentLoginOutcome) -> (&'static str, String) {
    if outcome.success {
        ("success", "Signed in.".to_string())
    } else {
        let detail = outcome
            .message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("sign-in did not complete");
        ("error", format!("Sign-in failed: {detail}"))
    }
}

async fn start_login(
    launcher_id: Uuid,
    agent_type: AgentType,
) -> Result<StartAgentLoginResponse, String> {
    let url = utils::api_url(&format!("/api/launchers/{launcher_id}/agent-login/start"));
    let body = StartAgentLoginRequest { agent_type };
    let resp = Request::post(&url)
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(error_body(resp).await);
    }
    resp.json::<StartAgentLoginResponse>()
        .await
        .map_err(|e| e.to_string())
}

async fn submit_code(
    launcher_id: Uuid,
    flow_id: Uuid,
    code: String,
) -> Result<AgentLoginOutcome, String> {
    let url = utils::api_url(&format!(
        "/api/launchers/{launcher_id}/agent-login/{flow_id}/code"
    ));
    let body = SubmitAgentLoginCodeRequest { code };
    let resp = Request::post(&url)
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(error_body(resp).await);
    }
    resp.json::<AgentLoginOutcome>()
        .await
        .map_err(|e| e.to_string())
}

async fn poll_login(launcher_id: Uuid, flow_id: Uuid) -> Result<AgentLoginOutcome, String> {
    let url = utils::api_url(&format!(
        "/api/launchers/{launcher_id}/agent-login/{flow_id}"
    ));
    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(error_body(resp).await);
    }
    resp.json::<AgentLoginOutcome>()
        .await
        .map_err(|e| e.to_string())
}

/// Best-effort fire-and-forget cancel; a dropped launcher flow reaps on its own
/// deadline, so we don't surface failures here.
fn cancel_login(launcher_id: Uuid, flow_id: Uuid) {
    spawn_local(async move {
        let url = utils::api_url(&format!(
            "/api/launchers/{launcher_id}/agent-login/{flow_id}/cancel"
        ));
        let _ = Request::post(&url).send().await;
    });
}

/// Read a non-2xx response body as the error message, falling back to the
/// status. The backend relays the agent CLI's own text on login-start failures.
async fn error_body(resp: gloo_net::http::Response) -> String {
    let status = resp.status();
    match resp.text().await {
        Ok(t) if !t.trim().is_empty() => t,
        _ => format!("HTTP {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_reads_cleanly() {
        let (class, text) = outcome_status(&AgentLoginOutcome {
            done: true,
            success: true,
            message: None,
        });
        assert_eq!(class, "success");
        assert_eq!(text, "Signed in.");
    }

    #[test]
    fn failure_surfaces_the_cli_message() {
        let (class, text) = outcome_status(&AgentLoginOutcome {
            done: true,
            success: false,
            message: Some("invalid code".to_string()),
        });
        assert_eq!(class, "error");
        assert_eq!(text, "Sign-in failed: invalid code");
    }

    #[test]
    fn failure_without_a_message_has_a_fallback() {
        let (_, text) = outcome_status(&AgentLoginOutcome {
            done: true,
            success: false,
            message: Some("   ".to_string()),
        });
        assert_eq!(text, "Sign-in failed: sign-in did not complete");
    }
}
