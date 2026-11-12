//! Provider verification happens outside the transcript. Completion notifications
//! only trigger a server status check; they never authorize an approval themselves.
use crate::utils::{self, On401};
use gloo::{events::EventListener, timers::callback::Timeout};
use gloo_net::http::Request;
use shared::api::{SecretVerificationRedirect, SecretVerificationStart, SecretVerificationStatus};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    #[prop_or_default]
    pub on_verified: Callback<bool>,
}
pub struct SecretVerification {
    status: Option<SecretVerificationStatus>,
    error: Option<String>,
    busy: bool,
    in_flight: bool,
    attempt: u64,
    completed: bool,
    deadline: f64,
    popup: Option<web_sys::Window>,
    channel: Option<web_sys::BroadcastChannel>,
    listener: Option<EventListener>,
    _focus_listener: Option<EventListener>,
    timer: Option<Timeout>,
}
pub enum Msg {
    Start(String),
    Started(u64, Result<String, String>),
    Check,
    Checked(u64, bool, Result<SecretVerificationStatus, String>),
    Completed(u64),
    Cancel,
}
impl SecretVerification {
    fn close(&mut self) {
        self.timer = None;
        self.listener = None;
        if let Some(channel) = self.channel.take() {
            channel.close();
        }
        if let Some(popup) = self.popup.take() {
            let _ = popup.close();
        }
        self.busy = false;
    }
    fn check(&mut self, ctx: &Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        let (attempt, completed) = (self.attempt, self.completed);
        ctx.link().send_future(async move {
            let result = utils::fetch_json::<SecretVerificationStatus>(
                "/api/secrets/verification",
                On401::Ignore,
            )
            .await
            .map_err(|_| "Could not check account verification. Try again.".into());
            Msg::Checked(attempt, completed, result)
        });
    }
}
impl Component for SecretVerification {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Check);
        let link = ctx.link().clone();
        let focus_listener = web_sys::window().map(|window| {
            EventListener::new(&window, "focus", move |_| link.send_message(Msg::Check))
        });
        Self {
            status: None,
            error: None,
            busy: false,
            in_flight: false,
            attempt: 0,
            completed: false,
            deadline: 0.0,
            popup: None,
            channel: None,
            listener: None,
            _focus_listener: focus_listener,
            timer: None,
        }
    }
    fn destroy(&mut self, _: &Context<Self>) {
        self.close();
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Start(provider) => {
                if self.busy {
                    return false;
                }
                // Open synchronously in the click handler, before HTTP awaits,
                // so popup blockers recognize the user's gesture. Never navigate
                // the original tab when opening a popup is denied.
                let popup = web_sys::window().and_then(|w| {
                    w.open_with_url_and_target_and_features(
                        "about:blank",
                        "_blank",
                        "popup,width=540,height=720",
                    )
                    .ok()
                    .flatten()
                });
                let Some(popup) = popup else {
                    self.error = Some("Allow popups for Portal, then try Verify again. Your session has stayed here.".into());
                    return true;
                };
                // No opener access is needed: a scoped BroadcastChannel and
                // server polling work even when a provider uses COOP isolation.
                if popup.set_opener(&wasm_bindgen::JsValue::NULL).is_err() {
                    let _ = popup.close();
                    self.error =
                        Some("Could not open a secure verification window. Try again.".into());
                    return true;
                }
                self.attempt += 1;
                self.in_flight = false;
                self.completed = false;
                self.busy = true;
                self.error = None;
                self.deadline = js_sys::Date::now() + 600_000.0;
                self.popup = Some(popup);
                let attempt = self.attempt;
                let channel_name = format!("portal-secret-verification-{}", uuid::Uuid::new_v4());
                if let Ok(channel) = web_sys::BroadcastChannel::new(&channel_name) {
                    let link = ctx.link().clone();
                    self.listener = Some(EventListener::new(&channel, "message", move |_| {
                        link.send_message(Msg::Completed(attempt))
                    }));
                    self.channel = Some(channel);
                }
                ctx.link().send_future(async move {
                    let request = Request::post(&utils::api_url("/api/secrets/verification/start"))
                        .header("x-portal-secrets", "1");
                    let body = SecretVerificationStart {
                        provider,
                        return_to: format!("/secrets/verified?channel={channel_name}"),
                    };
                    let result = async {
                        let response = utils::send_json(request, &body)
                            .await
                            .map_err(|_| "Could not start verification".to_string())?;
                        if !response.ok() {
                            return Err(
                                "Verification unavailable for this account. Try again.".into()
                            );
                        }
                        response
                            .json::<SecretVerificationRedirect>()
                            .await
                            .map(|r| r.url)
                            .map_err(|_| "Invalid verification response".into())
                    }
                    .await;
                    Msg::Started(attempt, result)
                });
            }
            Msg::Started(attempt, result) if attempt == self.attempt && self.busy => match result {
                Ok(url) => {
                    if self
                        .popup
                        .as_ref()
                        .is_none_or(|p| p.location().set_href(&url).is_err())
                    {
                        self.close();
                        self.error =
                            Some("Could not navigate the verification window. Try again.".into());
                    } else {
                        self.check(ctx);
                    }
                }
                Err(error) => {
                    self.close();
                    self.error = Some(error);
                }
            },
            Msg::Check => {
                if self.busy && js_sys::Date::now() >= self.deadline {
                    self.close();
                    self.error =
                        Some("Verification was not completed. Try again when ready.".into());
                } else {
                    self.check(ctx);
                }
            }
            Msg::Checked(attempt, after_completion, result) if attempt == self.attempt => {
                self.in_flight = false;
                match result {
                    Ok(value) => {
                        self.error = None;
                        ctx.props().on_verified.emit(value.verified);
                        if value.verified {
                            self.close();
                            self.error = None;
                        } else if self.busy && self.completed && after_completion {
                            self.close();
                            self.error = Some("Verification was cancelled or used a different account. Try again with your Portal account.".into());
                        }
                        self.status = Some(value);
                    }
                    Err(error) => self.error = Some(error),
                }
                if self.busy {
                    let link = ctx.link().clone();
                    self.timer = Some(Timeout::new(1_500, move || link.send_message(Msg::Check)));
                }
            }
            Msg::Completed(attempt) if attempt == self.attempt && self.busy => {
                self.completed = true;
                self.timer = None;
                self.check(ctx);
            }
            Msg::Cancel => {
                self.close();
                self.attempt += 1;
                self.in_flight = false;
                self.error = None;
            }
            _ => return false,
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let verified = self.status.as_ref().is_some_and(|s| s.verified);
        html! {<section class={classes!("secrets-verification", verified.then_some("is-verified"))} aria-label="Verify your account">
            if verified {<p role="status">{"Account verified"}</p>}
            else if self.busy {
                <p role="status">{"Finish verification in the new window. This session will stay open."}</p>
                <button type="button" class="secret-button" onclick={ctx.link().callback(|_| Msg::Cancel)}>{"Cancel verification"}</button>
            } else if let Some(status) = &self.status {
                <p>{"Verify your account to enable approval. Verification opens in a separate window."}</p>
                <div class="secret-access-actions">{for status.providers.iter().map(|provider| {
                    let provider=provider.clone(); let label=if provider=="google" {"Verify with Google"} else {"Verify with GitHub"};
                    html!{<button type="button" class="secret-button secret-button--primary" onclick={ctx.link().callback(move |_|Msg::Start(provider.clone()))}>{label}</button>}
                })}</div>
                if status.providers.is_empty() {<p>{"No linked login provider is configured for verification."}</p>}
            } else {<p role="status">{"Checking account verification…"}</p>}
            if let Some(error)=&self.error {<p role="alert">{error}</p>
                if !self.busy && self.status.is_none() {<button type="button" class="secret-button" onclick={ctx.link().callback(|_|Msg::Check)}>{"Retry"}</button>}
            }
        </section>}
    }
}
