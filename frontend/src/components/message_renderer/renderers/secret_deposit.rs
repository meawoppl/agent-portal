//! Metadata-only proposal review. Values never enter this component.
use crate::{portal_client::use_portal, utils};
use gloo::{timers::callback::Interval, timers::future::TimeoutFuture};
use gloo_net::http::Request;
use shared::api::*;
use uuid::Uuid;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub request_id: Uuid,
    pub session_id: Uuid,
}
#[function_component(SecretDepositCard)]
pub fn card(props: &Props) -> Html {
    let portal = use_portal();
    let location = use_location();
    let eligible = !portal.is_linked()
        && portal.can_write()
        && !location.is_some_and(|l| l.path().starts_with("/history"));
    let key = format!(
        "{}:{}:{:?}:{eligible}",
        props.request_id,
        portal.origin(),
        portal.account_id()
    );
    html! {<Review {key} request_id={props.request_id} session_id={props.session_id} {eligible}/>}
}
#[derive(Properties, PartialEq)]
struct ReviewProps {
    request_id: Uuid,
    session_id: Uuid,
    eligible: bool,
}
async fn fetch(id: Uuid) -> Result<SecretDepositInfo, &'static str> {
    let response = Request::get(&utils::api_url(&format!("/api/secrets/deposits/{id}")))
        .send()
        .await
        .map_err(|_| "Could not load this proposal.")?;
    if !response.ok() {
        return Err("Only the session owner can review this proposal. Check your account.");
    }
    response
        .json()
        .await
        .map_err(|_| "Invalid proposal response.")
}
async fn bounded<F, T>(future: F) -> Result<T, &'static str>
where
    F: std::future::Future<Output = Result<T, &'static str>>,
{
    let timeout = TimeoutFuture::new(15000);
    futures_util::pin_mut!(future, timeout);
    match futures_util::future::select(future, timeout).await {
        futures_util::future::Either::Left((result, _)) => result,
        _ => Err("Request timed out. Its status will be checked before retrying."),
    }
}
fn edits(info: &SecretDepositInfo) -> Vec<SecretDepositEdit> {
    info.items
        .iter()
        .map(|i| SecretDepositEdit {
            name: i.name.clone(),
            description: i.description.clone(),
            recipients: i.recipients.iter().map(|r| r.user_id).collect(),
        })
        .collect()
}
#[function_component(Review)]
fn review(props: &ReviewProps) -> Html {
    let info = use_state(|| None::<SecretDepositInfo>);
    let draft = use_state(Vec::<SecretDepositEdit>::new);
    let error = use_state(|| None::<&'static str>);
    let progress = use_state(|| None::<&'static str>);
    let busy = use_mut_ref(|| false);
    let tick = use_state(|| 0u64);
    let loading = use_mut_ref(|| false);
    let revision = use_mut_ref(|| 0u64);
    let alive = use_mut_ref(|| true);
    let expiry_seen = use_mut_ref(|| false);
    {
        let alive = alive.clone();
        use_effect_with((), move |_| {
            *alive.borrow_mut() = true;
            move || {
                *alive.borrow_mut() = false;
            }
        });
    }
    {
        let tick = tick.clone();
        use_effect_with(props.eligible, move |eligible| {
            let timer =
                eligible.then(|| Interval::new(2000, move || tick.set(js_sys::Date::now() as u64)));
            move || drop(timer)
        });
    }
    {
        let (info, draft, error, busy) = (info.clone(), draft.clone(), error.clone(), busy.clone());
        let (loading, revision, alive) = (loading.clone(), revision.clone(), alive.clone());
        let (id, session, eligible) = (props.request_id, props.session_id, props.eligible);
        use_effect_with((*tick, id, session, eligible), move |_| {
            if eligible
                && !*loading.borrow()
                && !*busy.borrow()
                && info.as_ref().is_none_or(|i| i.state == "pending")
            {
                *loading.borrow_mut() = true;
                let expected = *revision.borrow();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = bounded(fetch(id)).await;
                    *loading.borrow_mut() = false;
                    if !*alive.borrow() || *revision.borrow() != expected || *busy.borrow() {
                        return;
                    }
                    match result {
                        Ok(value) if value.id == id && value.session_id == session => {
                            if draft.is_empty() || value.state != "pending" {
                                draft.set(edits(&value));
                            }
                            info.set(Some(value));
                            error.set(None);
                        }
                        Ok(_) => {
                            info.set(None);
                            error.set(Some("Proposal belongs to another session."));
                        }
                        Err(e) => error.set(Some(e)),
                    }
                });
            }
            || ()
        });
    }
    let decide = {
        let revision = revision.clone();
        let (info, draft, error, progress, busy, alive) = (
            info.clone(),
            draft.clone(),
            error.clone(),
            progress.clone(),
            busy.clone(),
            alive.clone(),
        );
        let (id, session, eligible) = (props.request_id, props.session_id, props.eligible);
        Callback::from(move |approve: bool| {
            if !eligible || *busy.borrow() || info.as_ref().is_none_or(|i| i.state != "pending") {
                return;
            }
            *revision.borrow_mut() += 1;
            *busy.borrow_mut() = true;
            error.set(None);
            progress.set(Some(if approve {
                "Verifying Portal access…"
            } else {
                "Denying proposal…"
            }));
            let (info, error, progress, busy, alive) = (
                info.clone(),
                error.clone(),
                progress.clone(),
                busy.clone(),
                alive.clone(),
            );
            let decision = SecretDepositDecision {
                approve,
                items: approve.then(|| (*draft).clone()),
            };
            wasm_bindgen_futures::spawn_local(async move {
                let result = async {
                    if approve {
                        crate::components::secret_verification::verify_session().await?;
                    }
                    if !*alive.borrow() {
                        return Err("Review closed.");
                    }
                    progress.set(Some(if approve {
                        "Creating secrets…"
                    } else {
                        "Denying proposal…"
                    }));
                    let request = Request::post(&utils::api_url(&format!(
                        "/api/secrets/deposits/{id}/decision"
                    )))
                    .header("x-portal-secrets", "1")
                    .json(&decision)
                    .map_err(|_| "Could not prepare approval.")?;
                    let response = request
                        .send()
                        .await
                        .map_err(|_| "Could not confirm the decision. Checking status…")?;
                    if response.status() == 409 {
                        return Err("A name is unavailable or the proposal is no longer valid. Edit the name or submit a new proposal.");
                    }
                    if !response.ok() {
                        return Err("The decision was not accepted. Check your account and proposal status.");
                    }
                    response
                        .json::<SecretDepositInfo>()
                        .await
                        .map_err(|_| "Could not confirm the result.")
                };
                let result = bounded(result).await;
                if !*alive.borrow() {
                    return;
                }
                match result {
                    Ok(value) if value.id == id && value.session_id == session => {
                        info.set(Some(value))
                    }
                    Ok(_) => error.set(Some("Unexpected proposal response.")),
                    Err(e) => error.set(Some(e)),
                }
                progress.set(None);
                *busy.borrow_mut() = false;
            });
        })
    };
    if !props.eligible {
        return html! {<div class="permission-prompt secret-access-request">{"Open this session on its home portal to review the proposed secrets."}</div>};
    }
    let expired = info.as_ref().is_some_and(|i| {
        chrono::DateTime::parse_from_rfc3339(&i.expires_at)
            .ok()
            .is_some_and(|t| t.timestamp_millis() <= js_sys::Date::now() as i64)
    });
    if expired {
        *expiry_seen.borrow_mut() = true;
    }
    let expired = *expiry_seen.borrow();
    let pending = info.as_ref().is_some_and(|i| i.state == "pending") && !expired;
    let disabled = !pending || progress.is_some() || error.is_some();
    html! {
        <section class="permission-prompt secret-access-request secret-deposit-card" aria-busy={progress.is_some().to_string()}>
            <strong>{"Create proposed secrets"}</strong>
            <p class="secret-access-summary">{"Values are encrypted and hidden. Approval creates the entire batch; it does not grant the agent read access."}</p>
            {if let Some(value)=info.as_ref() {html!{
                <>
                {for value.items.iter().enumerate().map(|(index,item)| {
                    let edit=draft.get(index).cloned().unwrap_or_else(||SecretDepositEdit{name:item.name.clone(),description:item.description.clone(),recipients:item.recipients.iter().map(|r|r.user_id).collect()});
                    let onname={let draft=draft.clone(); Callback::from(move |e:InputEvent|{let mut next=(*draft).clone();if let Some(i)=next.get_mut(index){i.name=e.target_unchecked_into::<web_sys::HtmlInputElement>().value();}draft.set(next);})};
                    let ondescription={let draft=draft.clone(); Callback::from(move |e:InputEvent|{let mut next=(*draft).clone();if let Some(i)=next.get_mut(index){i.description=e.target_unchecked_into::<web_sys::HtmlInputElement>().value();}draft.set(next);})};
                    html!{<div class="secret-deposit-item">
                        <label>{if item.namespace==SecretNamespace::Personal {"Personal secret"} else {"Organization secret (all members can read/use)"}}
                            <input value={edit.name} oninput={onname} disabled={!pending || progress.is_some()} maxlength="128"/>
                        </label>
                        <label>{"Description"}<input value={edit.description} oninput={ondescription} disabled={!pending || progress.is_some()} maxlength="2048"/></label>
                        {for item.recipients.iter().map(|recipient| {
                            let draft=draft.clone();let id=recipient.user_id;
                            let checked=edit.recipients.contains(&id);
                            let onchange=Callback::from(move |e:Event| {let mut next=(*draft).clone();if let Some(i)=next.get_mut(index){i.recipients.retain(|u|*u!=id);if e.target_unchecked_into::<web_sys::HtmlInputElement>().checked(){i.recipients.push(id);}}draft.set(next);});
                            html!{<label class="secret-deposit-share"><input type="checkbox" {checked} {onchange} disabled={!pending || progress.is_some()}/><span>{format!("Share read/use with {} ({})",recipient.display_name,recipient.email)}</span></label>}
                        })}
                    </div>}
                })}
                {if pending {html!{<div class="secret-access-actions">
                    <button class="secret-button secret-button--primary" {disabled} onclick={let decide=decide.clone();Callback::from(move |_|decide.emit(true))}>{format!("Approve {} secret{}",value.items.len(),if value.items.len()==1 {""}else{"s"})}</button>
                    <button class="secret-button" {disabled} onclick={Callback::from(move |_|decide.emit(false))}>{"Deny"}</button>
                </div>}} else {html!{<p role="status">{if value.state=="pending" && expired {"Expired".to_string()} else {value.state.clone()}}</p>}}}
                </>
            }} else {html!{<p>{"Loading proposal…"}</p>}}}
            {if let Some(text)=*progress {html!{<p role="status" class="secret-access-progress"><span class="secret-verification-spinner" aria-hidden="true"/>{text}</p>}}else{Html::default()}}
            {if let Some(text)=*error {html!{<p role="alert">{text}</p>}}else{Html::default()}}
        </section>
    }
}
