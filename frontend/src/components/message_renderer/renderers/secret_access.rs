//! A transcript marker is only a pointer: authority and display metadata come from the API.
use crate::{portal_client::use_portal, utils};
use gloo::timers::callback::Timeout;
use gloo_net::http::Request;
use shared::api::{
    SecretAccessDecisionRequest, SecretAccessRequestInfo, SecretAccessState, SecretNamespace,
};
use uuid::Uuid;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub request_id: Uuid,
    pub session_id: Uuid,
}

#[function_component(SecretAccessRequestCard)]
pub fn secret_access_request_card(props: &Props) -> Html {
    let portal = use_portal();
    let location = use_location();
    let archived = location
        .as_ref()
        .is_some_and(|l| l.path().starts_with("/history"));
    let eligible = !archived && !portal.is_linked() && portal.can_write();
    // Remount on account/origin changes so verified data cannot cross an identity boundary.
    let key = format!(
        "{}:{}:{}:{:?}:{eligible}",
        props.request_id,
        props.session_id,
        portal.origin(),
        portal.account_id()
    );
    html! { <RequestCard {key} request_id={props.request_id} session_id={props.session_id} {eligible}/> }
}

#[derive(Properties, PartialEq)]
struct CardProps {
    request_id: Uuid,
    session_id: Uuid,
    eligible: bool,
}
struct RequestCard {
    info: Option<SecretAccessRequestInfo>,
    error: Option<&'static str>,
    busy: bool,
    poll: Option<Timeout>,
    account_verified: bool,
}
enum Msg {
    AccountVerified(bool),
    Load(bool),
    Loaded(Result<SecretAccessRequestInfo, &'static str>),
    Decide(bool),
    Decided(Result<(), &'static str>),
}
fn verified(info: &SecretAccessRequestInfo, request: Uuid, session: Uuid) -> bool {
    info.id == request && info.session_id == session
}
fn can_decide(eligible: bool, busy: bool, info: Option<&SecretAccessRequestInfo>) -> bool {
    eligible && !busy && pending(info)
}
fn inactive_notice() -> Html {
    html! { <p>{"Open this session on its home portal to review this request. Archived requests cannot be approved here."}</p> }
}
fn pending(info: Option<&SecretAccessRequestInfo>) -> bool {
    info.is_some_and(|i| i.state == SecretAccessState::Pending)
}
const TRANSIENT_ERROR: &str = "Could not refresh this request. Try again.";
async fn load(id: Uuid) -> Result<SecretAccessRequestInfo, &'static str> {
    let fetch = load_response(id);
    let timeout = gloo::timers::future::TimeoutFuture::new(10_000);
    futures_util::pin_mut!(fetch, timeout);
    match futures_util::future::select(fetch, timeout).await {
        futures_util::future::Either::Left((result, _)) => result,
        futures_util::future::Either::Right(_) => Err(TRANSIENT_ERROR),
    }
}
async fn load_response(id: Uuid) -> Result<SecretAccessRequestInfo, &'static str> {
    let response = Request::get(&utils::api_url(&format!("/api/secrets/requests/{id}")))
        .send()
        .await
        .map_err(|_| TRANSIENT_ERROR)?;
    match response.status() {
        200 => response.json().await.map_err(|_| "The server returned an invalid request."),
        401 | 403 => Err("Only the session owner can review this request. Check that you are using the correct account."),
        404 | 410 => Err("This request is unavailable or has expired."),
        _ => Err(TRANSIENT_ERROR),
    }
}
impl Component for RequestCard {
    type Message = Msg;
    type Properties = CardProps;
    fn create(ctx: &Context<Self>) -> Self {
        if ctx.props().eligible {
            ctx.link().send_message(Msg::Load(false));
        }
        Self {
            info: None,
            error: None,
            busy: false,
            poll: None,
            account_verified: false,
        }
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::AccountVerified(value) => self.account_verified = value,
            Msg::Load(preserve_error) => {
                if self.busy || !ctx.props().eligible {
                    return false;
                }
                self.poll = None;
                if !preserve_error {
                    self.error = None;
                }
                self.busy = true;
                let id = ctx.props().request_id;
                ctx.link()
                    .send_future(async move { Msg::Loaded(load(id).await) });
            }
            Msg::Loaded(result) => {
                self.busy = false;
                match result {
                    Ok(info) if verified(&info, ctx.props().request_id, ctx.props().session_id) => {
                        self.info = Some(info);
                        if pending(self.info.as_ref()) {
                            let link = ctx.link().clone();
                            self.poll = Some(Timeout::new(5_000, move || {
                                link.send_message(Msg::Load(false))
                            }));
                        }
                    }
                    Ok(_) => {
                        self.info = None;
                        self.error = Some("This request does not belong to this session.");
                    }
                    Err(error) => {
                        self.error = Some(error);
                        if error == TRANSIENT_ERROR && pending(self.info.as_ref()) {
                            // Keep polling a previously verified pending request across a reconnect,
                            // but never permit a decision while its status is unconfirmed.
                            let link = ctx.link().clone();
                            self.poll = Some(Timeout::new(5_000, move || {
                                link.send_message(Msg::Load(false))
                            }));
                        } else {
                            self.info = None;
                        }
                    }
                }
            }
            Msg::Decide(approve) => {
                if (approve && !self.account_verified)
                    || self.error.is_some()
                    || !can_decide(ctx.props().eligible, self.busy, self.info.as_ref())
                {
                    return false;
                }
                self.poll = None;
                self.busy = true;
                self.error = None;
                let id = ctx.props().request_id;
                ctx.link().send_future(async move {
                    let request = Request::post(&utils::api_url(&format!("/api/secrets/requests/{id}/decision")))
                        .header("x-portal-secrets", "1")
                        .header("Idempotency-Key", &Uuid::new_v4().to_string());
                    let result = utils::send_json(request, &SecretAccessDecisionRequest { approve }).await;
                    Msg::Decided(match result {
                        Ok(r) if r.ok() => Ok(()),
                        Ok(r) if r.status() == 401 || r.status() == 403 => Err("Approval requires the session owner and account verification. Use the verification button, then retry."),
                        _ => Err("The decision was not confirmed. Check the refreshed status before trying again."),
                    })
                });
            }
            Msg::Decided(result) => {
                self.busy = false;
                self.error = result.err();
                // Never trust a stale marker or a POST response as current authority.
                ctx.link().send_message(Msg::Load(true));
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let eligible = ctx.props().eligible;
        let actionable =
            self.error.is_none() && can_decide(eligible, self.busy, self.info.as_ref());
        let state_class = match self.info.as_ref().map(|i| &i.state) {
            Some(SecretAccessState::Approved) => "is-approved",
            Some(
                SecretAccessState::Denied
                | SecretAccessState::Expired
                | SecretAccessState::Invalidated,
            ) => "is-closed",
            _ => "is-pending",
        };
        html! { <section class={classes!("permission-prompt", "secret-access-request", state_class)} aria-live="polite">
            <strong>{"Agent secret access"}</strong>
            if !eligible { {inactive_notice()} }
            else {
                if let Some(info) = &self.info { {render_info(info)} }
                else if self.busy { <p>{"Checking request status…"}</p> }
                if let Some(error) = self.error {
                    <p role="alert">{error}</p>
                }
                if pending(self.info.as_ref()) {
                    <crate::components::secret_verification::SecretVerification on_verified={ctx.link().callback(Msg::AccountVerified)} />
                }
                <div class="secret-access-actions">
                    if pending(self.info.as_ref()) {
                        <button type="button" class="secret-button secret-button--primary" disabled={!actionable || !self.account_verified} onclick={ctx.link().callback(|_| Msg::Decide(true))}>{"Approve access"}</button>
                        <button type="button" class="secret-button" disabled={!actionable} onclick={ctx.link().callback(|_| Msg::Decide(false))}>{"Deny"}</button>
                    }
                    <button type="button" class="secret-button secret-button--quiet" disabled={self.busy} onclick={ctx.link().callback(|_| Msg::Load(false))}>{if self.busy {"Checking…"} else {"Refresh status"}}</button>
                </div>
            }
        </section> }
    }
}
fn render_info(info: &SecretAccessRequestInfo) -> Html {
    let namespace = match info.namespace {
        SecretNamespace::Personal => "Personal",
        SecretNamespace::Organization => "Organization",
    };
    let state = match info.state {
        SecretAccessState::Pending => "Pending",
        SecretAccessState::Approved => "Approved",
        SecretAccessState::Denied => "Denied",
        SecretAccessState::Expired => "Expired",
        SecretAccessState::Invalidated => "Invalidated",
    };
    let duration = if info.expires_in_seconds >= 60 && info.expires_in_seconds % 60 == 0 {
        let minutes = info.expires_in_seconds / 60;
        format!(
            "{minutes} {}",
            if minutes == 1 { "minute" } else { "minutes" }
        )
    } else {
        format!("{} seconds", info.expires_in_seconds)
    };
    let expiry = chrono::DateTime::parse_from_rfc3339(&info.expires_at)
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .format("%b %-d, %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| info.expires_at.clone());
    html! { <>
        <div class="secret-access-summary"><p><strong>{&info.name}</strong><span class="secret-access-version">{format!("{namespace} · version {}", info.version)}</span></p><span class="secret-access-status">{state}</span></div>
        <dl>
            <dt>{"Environment variable"}</dt><dd><code>{&info.env_name}</code></dd>
            <dt>{"Access duration"}</dt><dd>{duration}</dd>
            <dt>{if info.binding_id.is_some() {"Access expires"} else {"Request expires"}}</dt><dd><time datetime={info.expires_at.clone()}>{expiry}</time></dd>
        </dl>
        <details class="secret-access-details"><summary>{"Technical details"}</summary><dl>
            <dt>{"Owner"}</dt><dd>{info.owner_user_id.to_string()}</dd>
            <dt>{"Secret ID"}</dt><dd>{info.secret_id.to_string()}</dd>
            <dt>{"Request ID"}</dt><dd>{info.id.to_string()}</dd>
            <dt>{"Session"}</dt><dd>{info.session_id.to_string()}</dd>
            <dt>{"Launcher"}</dt><dd>{info.launcher_id.to_string()}</dd>
        </dl></details>
        if info.state == SecretAccessState::Pending {
            <p>{"Approval gives this agent process plaintext access to this secret version. The value stays hidden here."}</p>
        }
    </> }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info() -> SecretAccessRequestInfo {
        SecretAccessRequestInfo {
            id: Uuid::new_v4(),
            secret_id: Uuid::new_v4(),
            owner_user_id: Uuid::new_v4(),
            namespace: SecretNamespace::Personal,
            name: "deploy-key".into(),
            version: 3,
            session_id: Uuid::new_v4(),
            launcher_id: Uuid::new_v4(),
            env_name: "DEPLOY_KEY".into(),
            expires_in_seconds: 900,
            state: SecretAccessState::Pending,
            created_at: "2026-11-11T00:00:00Z".into(),
            expires_at: "2026-11-11T00:10:00Z".into(),
            binding_id: None,
        }
    }
    #[test]
    fn binding_expiry_is_distinct_from_pending_request_deadline() {
        let mut info = info();
        let pending = super::super::visible_text(&render_info(&info));
        assert!(pending.contains("Request expires"));
        info.state = SecretAccessState::Approved;
        info.binding_id = Some(Uuid::new_v4());
        let approved = super::super::visible_text(&render_info(&info));
        assert!(approved.contains("Access expires"));
        assert!(!approved.contains("Request expires"));
    }
    #[test]
    fn metadata_requires_both_request_and_session_identity() {
        let info = info();
        assert!(verified(&info, info.id, info.session_id));
        assert!(!verified(&info, Uuid::new_v4(), info.session_id));
        assert!(!verified(&info, info.id, Uuid::new_v4()));
    }
    #[test]
    fn only_verified_pending_state_can_offer_a_decision() {
        assert!(!pending(None));
        let mut info = info();
        assert!(pending(Some(&info)));
        assert!(can_decide(true, false, Some(&info)));
        assert!(!can_decide(false, false, Some(&info)));
        assert!(!can_decide(true, true, Some(&info)));
        for state in [
            SecretAccessState::Approved,
            SecretAccessState::Denied,
            SecretAccessState::Expired,
            SecretAccessState::Invalidated,
        ] {
            info.state = state;
            assert!(!pending(Some(&info)));
        }
    }
    #[test]
    fn inactive_cards_have_no_agent_supplied_metadata_or_controls() {
        let text = super::super::visible_text(&inactive_notice());
        assert!(text.contains("Archived requests cannot be approved"));
        assert!(!text.contains("deploy-key"));
        assert!(!text.contains("Approve plaintext access"));
    }
    #[test]
    fn renders_authoritative_scope_and_plaintext_warning_without_a_saved_value() {
        let info = info();
        let text = super::super::visible_text(&render_info(&info));
        for expected in [
            "Personal / deploy-key",
            "version 3",
            "DEPLOY_KEY",
            "900 seconds",
            "plaintext access",
            &info.owner_user_id.to_string(),
            &info.session_id.to_string(),
            &info.launcher_id.to_string(),
            &info.secret_id.to_string(),
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("password"));
        assert!(!text.contains("Copy"));
        assert!(!text.contains("Reveal"));
    }
}
