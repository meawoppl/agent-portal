//! Interactive agent sign-in modal (Settings ▸ Agents).
//!
//! Drives one launcher-side login flow to completion over the four backend
//! relay endpoints (`/api/launchers/{id}/agent-login/...`). The two agents take
//! different shapes, expressed by [`LoginInteraction`]:
//!
//! - **claude** ([`LoginInteraction::SubmitCode`]): show the auth URL, let the
//!   user approve in the browser or paste a fallback code, and poll completion.
//! - **codex** ([`LoginInteraction::AwaitCompletion`]): show the device code +
//!   verification URL, then poll until the browser approval lands.
//!
//! Closing the modal before the flow settles cancels it launcher-side
//! (fire-and-forget), so a parked PTY / app-server never lingers. A successful
//! sign-in fires `on_success` so the matrix can re-probe.

use shared::api::{StartAgentLoginRequest, StartAgentLoginResponse, SubmitAgentLoginCodeRequest};
use shared::{AgentLoginOutcome, AgentType, LoginInteraction, LoginPresentable};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::components::DismissibleBackdrop;

use crate::portal_client::{portal_from_context, PortalClient};
use crate::utils;

#[derive(Properties, PartialEq)]
struct LoginLinkActionsProps {
    url: AttrValue,
}

#[derive(Clone, Copy, PartialEq)]
enum CopyState {
    Ready,
    Copying,
    Copied,
    Failed,
}

/// Keep the full OAuth URL in the link/clipboard rather than making the modal
/// lay out its long query string. Only report success after the clipboard write.
#[function_component(LoginLinkActions)]
fn login_link_actions(props: &LoginLinkActionsProps) -> Html {
    let state = use_state(|| CopyState::Ready);
    let on_copy = {
        let url = props.url.clone();
        let state = state.clone();
        Callback::from(move |_| {
            let state = state.clone();
            let url = url.clone();
            state.set(CopyState::Copying);
            spawn_local(async move {
                let copied = if let Some(window) = web_sys::window() {
                    wasm_bindgen_futures::JsFuture::from(
                        window.navigator().clipboard().write_text(&url),
                    )
                    .await
                    .is_ok()
                } else {
                    false
                };
                state.set(if copied {
                    CopyState::Copied
                } else {
                    CopyState::Failed
                });
            });
        })
    };
    html! {
        <div class="agent-login-link-group">
            <div class="agent-login-link-actions">
                <a class="agent-login-link-button" href={props.url.clone()} target="_blank" rel="noopener noreferrer"
                    title="Open login page in a new tab">
                    { "Open login" }
                </a>
                <button type="button" class="agent-login-link-button" onclick={on_copy}
                    disabled={*state == CopyState::Copying}>
                    { if *state == CopyState::Copied { "Copied!" } else { "Copy link" } }
                </button>
            </div>
            <span class="agent-login-copy-status" role="status">
                { match *state {
                    CopyState::Failed => "Could not copy. Use the Open login link's context menu to copy its address.",
                    CopyState::Copied => "Login link copied.",
                    _ => "",
                } }
            </span>
        </div>
    }
}

/// How often to poll browser approval, in milliseconds.
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
    /// Flow started; waiting on browser approval or a fallback code.
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
        let portal = portal_from_context(ctx);
        ctx.link().send_future(async move {
            Msg::Started(start_login(portal, launcher_id, agent_type).await)
        });
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
                // Both agents can complete in the browser without a pasted code.
                self.schedule_poll(ctx);
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
                let portal = portal_from_context(ctx);
                ctx.link().send_future(async move {
                    match submit_code(portal, launcher_id, flow_id, code).await {
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
                if !matches!(self.stage, Stage::Presenting { .. }) || self.submitting {
                    return false;
                }
                let Some(flow_id) = self.flow_id else {
                    return false;
                };
                let launcher_id = ctx.props().launcher_id;
                let portal = portal_from_context(ctx);
                ctx.link().send_future(async move {
                    Msg::Polled(poll_login(portal, launcher_id, flow_id).await)
                });
                false
            }
            // A successful poll may race a fallback submission. The launcher
            // removes completed flows, so the other request can report "gone";
            // preserve the confirmed approval regardless of response order.
            Msg::Polled(Ok(outcome)) if outcome.done && outcome.success => {
                self.settle(outcome, &ctx.props().on_success, &ctx.props().on_close);
                true
            }
            Msg::Polled(_)
                if !matches!(self.stage, Stage::Presenting { .. }) || self.submitting =>
            {
                false
            }
            Msg::Polled(Ok(outcome)) => {
                if outcome.done {
                    self.settle(outcome, &ctx.props().on_success, &ctx.props().on_close);
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
            Msg::Settled(_) if matches!(self.stage, Stage::Settled(_)) => false,
            Msg::Settled(outcome) => {
                self.submitting = false;
                self.settle(outcome, &ctx.props().on_success, &ctx.props().on_close);
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
        let on_close = ctx.link().callback(|()| Msg::Close);

        html! {
            <DismissibleBackdrop class="agent-login-overlay" on_close={on_close.clone()}>
                <div class="agent-login-modal">
                    <div class="agent-login-header">
                        <h2>{ format!("Sign in to {}", ctx.props().agent_name) }</h2>
                        <button class="agent-login-close" onclick={on_close.clone().reform(|_: MouseEvent| ())}>{ "×" }</button>
                    </div>
                    <div class="agent-login-body">
                        { self.view_stage(ctx) }
                    </div>
                    <div class="agent-login-footer">
                        <button class="link-button" onclick={on_close.reform(|_: MouseEvent| ())}>{ self.close_label() }</button>
                    </div>
                </div>
            </DismissibleBackdrop>
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

    /// Settle before notifying the parent: success refreshes the matrix and
    /// closes the modal, and must not be cancelled as an unfinished flow.
    fn settle(
        &mut self,
        outcome: AgentLoginOutcome,
        on_success: &Callback<()>,
        on_close: &Callback<()>,
    ) {
        let notify = outcome.done && outcome.success && !self.notified_success;
        self.stage = Stage::Settled(outcome);
        if notify {
            self.notified_success = true;
            on_success.emit(());
            on_close.emit(());
        }
    }

    /// Cancel the launcher-side flow if it's still parked (browser closed
    /// mid-flow). No-op once settled — the launcher already dropped it.
    fn cancel_if_unsettled(&self, ctx: &Context<Self>) {
        if matches!(self.stage, Stage::Settled(_) | Stage::Failed(_)) {
            return;
        }
        if let Some(flow_id) = self.flow_id {
            cancel_login(&portal_from_context(ctx), ctx.props().launcher_id, flow_id);
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
                            { "Open the login page and approve the sign-in. This window will update automatically. If prompted, paste the code below:" }
                        </p>
                        <LoginLinkActions key={url.clone()} url={url.clone()} />
                        <div class="agent-login-code-row">
                            <input
                                type="text"
                                class="agent-login-code-input"
                                placeholder="Paste code here if prompted"
                                value={self.code_input.clone()}
                                oninput={on_input}
                                onkeypress={on_key}
                                disabled={self.submitting}
                            />
                            <button
                                class="agent-login-submit"
                                onclick={on_submit}
                                disabled={self.submitting || !utils::is_non_blank(&self.code_input)}
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
                        { "Open the login page and enter the code to approve the sign-in:" }
                    </p>
                    <LoginLinkActions key={verification_url.clone()} url={verification_url.clone()} />
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
            .filter(|m| utils::is_non_blank(m))
            .unwrap_or("sign-in did not complete");
        ("error", format!("Sign-in failed: {detail}"))
    }
}

async fn start_login(
    portal: PortalClient,
    launcher_id: Uuid,
    agent_type: AgentType,
) -> Result<StartAgentLoginResponse, String> {
    if portal.is_linked() {
        return Err("Sign in from the remote portal's Computers page".into());
    }
    match crate::dashboard_rpc::request(shared::api::DashboardRequest::StartAgentLogin {
        launcher_id,
        request: StartAgentLoginRequest { agent_type },
    })
    .await
    .map_err(|e| e.message)?
    {
        shared::api::DashboardResponse::LoginStarted(response) => Ok(response),
        _ => Err("Unexpected login reply".into()),
    }
}

async fn submit_code(
    portal: PortalClient,
    launcher_id: Uuid,
    flow_id: Uuid,
    code: String,
) -> Result<AgentLoginOutcome, String> {
    if portal.is_linked() {
        return Err("Sign in from the remote portal's Computers page".into());
    }
    match crate::dashboard_rpc::request(shared::api::DashboardRequest::SubmitAgentLoginCode {
        launcher_id,
        flow_id,
        request: SubmitAgentLoginCodeRequest { code },
    })
    .await
    .map_err(|e| e.message)?
    {
        shared::api::DashboardResponse::LoginOutcome(response) => Ok(response),
        _ => Err("Unexpected login reply".into()),
    }
}

async fn poll_login(
    portal: PortalClient,
    launcher_id: Uuid,
    flow_id: Uuid,
) -> Result<AgentLoginOutcome, String> {
    if portal.is_linked() {
        return Err("Sign in from the remote portal's Computers page".into());
    }
    match crate::dashboard_rpc::request(shared::api::DashboardRequest::PollAgentLogin {
        launcher_id,
        flow_id,
    })
    .await
    .map_err(|e| e.message)?
    {
        shared::api::DashboardResponse::LoginOutcome(response) => Ok(response),
        _ => Err("Unexpected login reply".into()),
    }
}

/// Best-effort fire-and-forget cancel; a dropped launcher flow reaps on its own
/// deadline, so we don't surface failures here.
fn cancel_login(portal: &PortalClient, launcher_id: Uuid, flow_id: Uuid) {
    if portal.is_linked() {
        return;
    }
    spawn_local(async move {
        let _ = crate::dashboard_rpc::request(shared::api::DashboardRequest::CancelAgentLogin {
            launcher_id,
            flow_id,
        })
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_login_refreshes_then_closes_once_and_failure_stays_open() {
        use std::{cell::RefCell, rc::Rc};
        let events = Rc::new(RefCell::new(Vec::new()));
        let success = {
            let events = events.clone();
            Callback::from(move |_| events.borrow_mut().push("refresh"))
        };
        let close = {
            let events = events.clone();
            Callback::from(move |_| events.borrow_mut().push("close"))
        };
        let mut modal = AgentLoginModal {
            stage: Stage::Starting,
            flow_id: None,
            code_input: String::new(),
            submitting: false,
            notified_success: false,
        };
        modal.settle(
            AgentLoginOutcome {
                done: true,
                success: false,
                message: None,
            },
            &success,
            &close,
        );
        assert!(events.borrow().is_empty());
        for _ in 0..2 {
            modal.settle(
                AgentLoginOutcome {
                    done: true,
                    success: true,
                    message: None,
                },
                &success,
                &close,
            );
        }
        assert_eq!(*events.borrow(), vec!["refresh", "close"]);
        assert!(matches!(modal.stage, Stage::Settled(_)));
    }

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
