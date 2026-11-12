//! A transcript marker is only a pointer: authority and display metadata come from the API.
use crate::{portal_client::use_portal, utils};
use gloo::{events::EventListener, timers::callback::Timeout};
use gloo_net::http::Request;
use shared::api::{
    SecretAccessDecisionRequest, SecretAccessGroupInfo, SecretAccessRequestInfo, SecretAccessState,
    SecretNamespace,
};
use uuid::Uuid;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub request_id: Uuid,
    pub session_id: Uuid,
    #[prop_or_default]
    pub grouped: bool,
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
    html! { <RequestCard {key} request_id={props.request_id} session_id={props.session_id} grouped={props.grouped} {eligible}/> }
}

#[derive(Properties, PartialEq)]
struct CardProps {
    request_id: Uuid,
    session_id: Uuid,
    eligible: bool,
    grouped: bool,
}
struct RequestCard {
    info: Option<SecretAccessGroupInfo>,
    error: Option<&'static str>,
    busy: bool,
    poll: Option<Timeout>,
    expiry: Option<Timeout>,
    _wake: Vec<EventListener>,
    account_verified: bool,
}
enum Msg {
    Expire,
    AccountVerified(bool),
    Load(bool),
    Loaded(Result<SecretAccessGroupInfo, &'static str>),
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
const TRANSIENT_ERROR: &str = "Could not check this request. Retrying automatically…";
fn active(state: SecretAccessState) -> bool {
    matches!(
        state,
        SecretAccessState::Pending | SecretAccessState::Approved
    )
}
fn deadline(item: &SecretAccessRequestInfo) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(&item.expires_at)
        .ok()
        .map(|t| t.timestamp_millis())
}
fn expire_at(info: &mut SecretAccessGroupInfo, now: i64) {
    if !active(info.state) {
        return;
    }
    for item in &mut info.items {
        if active(item.state) && deadline(item).is_some_and(|t| t <= now) {
            item.state = SecretAccessState::Expired;
        }
    }
    if info
        .items
        .iter()
        .any(|i| i.state == SecretAccessState::Expired)
    {
        info.state = SecretAccessState::Expired;
    }
}
fn can_decide_group(
    eligible: bool,
    busy: bool,
    info: Option<&SecretAccessGroupInfo>,
    now: i64,
) -> bool {
    info.is_some_and(|group| {
        group.state == SecretAccessState::Pending
            && !group.items.is_empty()
            && group.items.iter().all(|item| {
                can_decide(eligible, busy, Some(item)) && deadline(item).is_some_and(|t| t > now)
            })
    })
}
impl RequestCard {
    fn schedule_expiry(&mut self, ctx: &Context<Self>) {
        self.expiry = None;
        if let Some(info) = &mut self.info {
            let now = js_sys::Date::now() as i64;
            expire_at(info, now);
            if active(info.state) {
                if self._wake.is_empty() {
                    if let Some(window) = web_sys::window() {
                        let link = ctx.link().clone();
                        self._wake
                            .push(EventListener::new(&window, "focus", move |_| {
                                link.send_message(Msg::Expire)
                            }));
                        if let Some(document) = window.document() {
                            let link = ctx.link().clone();
                            self._wake.push(EventListener::new(
                                &document,
                                "visibilitychange",
                                move |_| link.send_message(Msg::Expire),
                            ));
                        }
                    }
                }
                if let Some(next) = info
                    .items
                    .iter()
                    .filter(|i| active(i.state))
                    .filter_map(deadline)
                    .min()
                {
                    let delay = (next - now).clamp(1, i32::MAX as i64) as u32;
                    let link = ctx.link().clone();
                    self.expiry = Some(Timeout::new(delay, move || link.send_message(Msg::Expire)));
                }
            } else {
                self.poll = None;
                self._wake.clear();
                if self.error == Some(TRANSIENT_ERROR) {
                    self.error = None;
                }
            }
        }
    }
}

async fn load(id: Uuid, grouped: bool) -> Result<SecretAccessGroupInfo, &'static str> {
    let fetch = load_response(id, grouped);
    let timeout = gloo::timers::future::TimeoutFuture::new(10_000);
    futures_util::pin_mut!(fetch, timeout);
    match futures_util::future::select(fetch, timeout).await {
        futures_util::future::Either::Left((result, _)) => result,
        futures_util::future::Either::Right(_) => Err(TRANSIENT_ERROR),
    }
}
async fn load_response(id: Uuid, grouped: bool) -> Result<SecretAccessGroupInfo, &'static str> {
    let response = Request::get(&utils::api_url(&format!(
        "/api/secrets/{}/{id}",
        if grouped {
            "request-groups"
        } else {
            "requests"
        }
    )))
    .send()
    .await
    .map_err(|_| TRANSIENT_ERROR)?;
    match response.status() {
        200 => {
            if grouped { response.json().await.map_err(|_| "The server returned an invalid request.") }
            else { let item: SecretAccessRequestInfo = response.json().await.map_err(|_| "The server returned an invalid request.")?;
                Ok(SecretAccessGroupInfo {id: item.id, state: item.state, items: vec![item]}) }
        },
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
            expiry: None,
            _wake: Vec::new(),
            account_verified: false,
        }
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Expire => self.schedule_expiry(ctx),
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
                let grouped = ctx.props().grouped;
                ctx.link()
                    .send_future(async move { Msg::Loaded(load(id, grouped).await) });
            }
            Msg::Loaded(result) => {
                self.busy = false;
                match result {
                    Ok(info)
                        if verified_group(
                            &info,
                            ctx.props().request_id,
                            ctx.props().session_id,
                            ctx.props().grouped,
                        ) =>
                    {
                        self.info = Some(info);
                        self.schedule_expiry(ctx);
                        if self
                            .info
                            .as_ref()
                            .is_some_and(|i| i.state == SecretAccessState::Pending)
                        {
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
                        if error == TRANSIENT_ERROR
                            && self
                                .info
                                .as_ref()
                                .is_some_and(|i| i.state == SecretAccessState::Expired)
                        {
                            // An overdue fetch cannot erase a locally expired card.
                            self.error = None;
                            return true;
                        }
                        self.error = Some(error);
                        if error == TRANSIENT_ERROR
                            && self
                                .info
                                .as_ref()
                                .is_none_or(|i| i.state == SecretAccessState::Pending)
                        {
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
                if self.info.is_none() {
                    self.expiry = None;
                    self._wake.clear();
                }
            }
            Msg::Decide(approve) => {
                if (approve && !self.account_verified)
                    || self.error.is_some()
                    || !can_decide_group(
                        ctx.props().eligible,
                        self.busy,
                        self.info.as_ref(),
                        js_sys::Date::now() as i64,
                    )
                {
                    return false;
                }
                self.poll = None;
                self.busy = true;
                self.error = None;
                let id = ctx.props().request_id;
                let grouped = ctx.props().grouped;
                ctx.link().send_future(async move {
                    let request = Request::post(&utils::api_url(&format!("/api/secrets/{}/{id}/decision", if grouped {"request-groups"} else {"requests"})))
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
        let actionable = self.error.is_none()
            && can_decide_group(
                eligible,
                self.busy,
                self.info.as_ref(),
                js_sys::Date::now() as i64,
            );
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
            <header class="secret-access-summary"><strong>{"Agent secret access"}</strong>
                if let Some(info) = &self.info {<span class="secret-access-status">{state_text(info.state)}</span>}
            </header>
            if !eligible { {inactive_notice()} }
            else {
                if let Some(info) = &self.info { {if ctx.props().grouped {render_group(info)} else {info.items.first().map(render_info).unwrap_or_default()}} }
                else if self.busy { <p>{"Checking request status…"}</p> }
                if let Some(error) = self.error {
                    <p role="alert">{error}</p>
                }
                if self.info.as_ref().is_some_and(|i| i.state == SecretAccessState::Pending) {
                    <crate::components::secret_verification::SecretVerification on_verified={ctx.link().callback(Msg::AccountVerified)} />
                }
                if self.info.as_ref().is_some_and(|i| i.state == SecretAccessState::Pending) {
                    <div class="secret-access-actions">
                        <button type="button" class="secret-button secret-button--primary" disabled={!actionable || !self.account_verified} onclick={ctx.link().callback(|_| Msg::Decide(true))}>{if ctx.props().grouped {"Approve all credentials"} else {"Approve access"}}</button>
                        <button type="button" class="secret-button" disabled={!actionable} onclick={ctx.link().callback(|_| Msg::Decide(false))}>{if ctx.props().grouped {"Deny all"} else {"Deny"}}</button>
                    </div>
                }
            }
        </section> }
    }
}
fn verified_group(
    info: &SecretAccessGroupInfo,
    request: Uuid,
    session: Uuid,
    grouped: bool,
) -> bool {
    if info.id != request
        || info.items.is_empty()
        || info.items.len() > shared::api::MAX_SECRET_ACCESS_GROUP_ITEMS
    {
        return false;
    }
    let first = &info.items[0];
    info.items.iter().all(|i| {
        deadline(i).is_some()
            && i.session_id == session
            && i.owner_user_id == first.owner_user_id
            && i.launcher_id == first.launcher_id
    }) && (grouped || (info.items.len() == 1 && verified(first, request, session)))
}
fn state_text(state: SecretAccessState) -> &'static str {
    match state {
        SecretAccessState::Pending => "Pending",
        SecretAccessState::Approved => "Approved",
        SecretAccessState::Denied => "Denied",
        SecretAccessState::Expired => "Expired",
        SecretAccessState::Invalidated => "Invalidated",
    }
}
fn render_credential(item: &SecretAccessRequestInfo) -> Html {
    html! {<div class="secret-access-credential">
        <div><strong>{&item.name}</strong><span class="secret-access-version">{format!("{} · version {}", if item.namespace == SecretNamespace::Personal {"Personal"} else {"Organization"}, item.version)}</span></div>
        <span class="secret-access-environment" title="Environment variable"><code>{&item.env_name}</code></span>
    </div>}
}
fn render_deadline(item: &SecretAccessRequestInfo) -> Html {
    html! {<p class="secret-access-timing">{format!("{} · {} ", duration_text(item.expires_in_seconds), if item.binding_id.is_some() {"Access expires"} else {"Request expires"})}<time datetime={item.expires_at.clone()}>{expiry_text(&item.expires_at)}</time></p>}
}
fn render_details(item: &SecretAccessRequestInfo) -> Html {
    html! {<dl>
        <dt>{"Owner"}</dt><dd>{item.owner_user_id.to_string()}</dd>
        <dt>{"Secret ID"}</dt><dd>{item.secret_id.to_string()}</dd>
        <dt>{"Request ID"}</dt><dd>{item.id.to_string()}</dd>
        <dt>{"Session"}</dt><dd>{item.session_id.to_string()}</dd>
        <dt>{"Launcher"}</dt><dd>{item.launcher_id.to_string()}</dd>
    </dl>}
}
fn render_group(info: &SecretAccessGroupInfo) -> Html {
    let first = &info.items[0]; // verified_group rejects empty responses.
    let common = info.items.iter().all(|i| {
        i.expires_at == first.expires_at
            && i.expires_in_seconds == first.expires_in_seconds
            && i.binding_id.is_some() == first.binding_id.is_some()
    });
    html! {<div class="secret-access-group">
        <ul>{for info.items.iter().map(|item| html!{<li>
            {render_credential(item)}
            if !common {{render_deadline(item)}}
        </li>})}</ul>
        if common {{render_deadline(first)}}
        <details class="secret-access-details"><summary>{"Technical details"}</summary>{for info.items.iter().map(|item| html!{<div><strong>{&item.name}</strong>{render_details(item)}</div>})}</details>
        if info.state == SecretAccessState::Pending {<p>{"Approve plaintext access to all listed credentials for this agent process. Values stay hidden here."}</p>}
    </div>}
}
fn duration_text(seconds: u64) -> String {
    if seconds >= 60 && seconds.is_multiple_of(60) {
        let minutes = seconds / 60;
        format!(
            "{minutes} {}",
            if minutes == 1 { "minute" } else { "minutes" }
        )
    } else {
        format!("{seconds} seconds")
    }
}
fn expiry_text(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%b %-d, %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| value.into())
}
fn render_info(info: &SecretAccessRequestInfo) -> Html {
    html! { <>
        {render_credential(info)}
        {render_deadline(info)}
        <details class="secret-access-details"><summary>{"Technical details"}</summary>{render_details(info)}</details>
        if info.state == SecretAccessState::Pending {
            <p>{"Approve plaintext access to this secret version for this agent process. The value stays hidden here."}</p>
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
    fn deadlines_expire_pending_and_approved_groups_without_reopening_closed_states() {
        for state in [SecretAccessState::Pending, SecretAccessState::Approved] {
            let mut item = info();
            item.state = state;
            let end = deadline(&item).unwrap();
            let mut group = SecretAccessGroupInfo {
                id: item.id,
                state,
                items: vec![item],
            };
            expire_at(&mut group, end - 1);
            assert_eq!(group.state, state);
            assert_eq!(
                can_decide_group(true, false, Some(&group), end - 1),
                state == SecretAccessState::Pending
            );
            assert!(!can_decide_group(true, false, Some(&group), end));
            expire_at(&mut group, end);
            assert_eq!(group.state, SecretAccessState::Expired);
            assert_eq!(group.items[0].state, SecretAccessState::Expired);
            expire_at(&mut group, end - 1000);
            assert_eq!(group.state, SecretAccessState::Expired);
        }
        for state in [SecretAccessState::Denied, SecretAccessState::Invalidated] {
            let mut item = info();
            item.state = state;
            let mut group = SecretAccessGroupInfo {
                id: item.id,
                state,
                items: vec![item],
            };
            expire_at(&mut group, i64::MAX);
            assert_eq!(group.state, state);
        }
    }
    #[test]
    fn group_uses_earliest_deadline_and_rejects_invalid_timestamps() {
        let first = info();
        let end = deadline(&first).unwrap();
        let mut second = first.clone();
        second.id = Uuid::new_v4();
        second.expires_at = "2026-11-11T00:20:00Z".into();
        let mut group = SecretAccessGroupInfo {
            id: Uuid::new_v4(),
            state: SecretAccessState::Pending,
            items: vec![first, second],
        };
        assert!(verified_group(
            &group,
            group.id,
            group.items[0].session_id,
            true
        ));
        expire_at(&mut group, end);
        assert_eq!(group.state, SecretAccessState::Expired);
        assert!(!can_decide_group(true, false, Some(&group), end));
        assert_eq!(group.items[1].state, SecretAccessState::Pending);
        group.items[1].expires_at = "invalid".into();
        assert!(!verified_group(
            &group,
            group.id,
            group.items[0].session_id,
            true
        ));
    }
    #[test]
    fn compact_group_shares_timing_but_preserves_every_credential_scope() {
        let first = info();
        let mut second = first.clone();
        second.name = "other-key".into();
        second.env_name = "OTHER_KEY".into();
        let group = SecretAccessGroupInfo {
            id: Uuid::new_v4(),
            state: SecretAccessState::Pending,
            items: vec![first, second],
        };
        let text = super::super::visible_text(&render_group(&group));
        assert_eq!(text.matches("15 minutes").count(), 1);
        for scope in [
            "deploy-key",
            "other-key",
            "DEPLOY_KEY",
            "OTHER_KEY",
            "plaintext access",
        ] {
            assert!(text.contains(scope));
        }
        assert_eq!(text.matches("Technical details").count(), 1);
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
            "Personal",
            "deploy-key",
            "version 3",
            "DEPLOY_KEY",
            "15 minutes",
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
