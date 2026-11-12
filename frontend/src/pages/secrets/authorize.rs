//! Enrollment approval never receives the CLI device code or credential.
use crate::Route;
use shared::api::SecretAuthorizeApproveRequest;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_router::prelude::*;

#[function_component(SecretAuthorizePage)]
pub fn secret_authorize_page() -> Html {
    let code = use_state(|| {
        web_sys::window()
            .and_then(|w| w.location().search().ok())
            .and_then(|query| {
                query
                    .trim_start_matches('?')
                    .split('&')
                    .find_map(|part| part.strip_prefix("code="))
                    .map(str::to_owned)
            })
            .and_then(|code| js_sys::decode_uri_component(&code).ok().map(String::from))
            .unwrap_or_default()
    });
    let confirmed = use_state(|| false);
    let verified = use_state(|| false);
    let busy = use_state(|| false);
    let result = use_state(|| None::<Result<(), String>>);
    let action = |approve: bool| {
        let (code, confirmed, busy, result) = (
            code.clone(),
            confirmed.clone(),
            busy.clone(),
            result.clone(),
        );
        Callback::from(move |_| {
            if *busy || (*code).trim().is_empty() || (approve && !*confirmed) {
                return;
            }
            busy.set(true);
            let request = SecretAuthorizeApproveRequest {
                user_code: code.trim().into(),
            };
            let (busy, result) = (busy.clone(), result.clone());
            spawn_local(async move {
                let path = if approve {
                    "/api/secrets/authorize/approve"
                } else {
                    "/api/secrets/authorize/deny"
                };
                let response = super::api::mutate("POST", path, &request).await.map(|_| ());
                result.set(Some(response));
                busy.set(false);
            });
        })
    };
    html! {<main class="secrets-page secrets-authorization"><h1>{"Authorize your CLI"}</h1>
        <p>{"Only approve a code from a command you started on a computer you trust. This grants that CLI access to secret metadata, reads, use and management for your account for up to 30 days. It does not grant access to other people's personal secrets."}</p>
        <crate::components::secret_verification::SecretVerification on_verified={{let verified=verified.clone(); Callback::from(move |value| verified.set(value))}} return_to={Some(format!("/secrets/authorize?code={}",js_sys::encode_uri_component(&code)))} />
        <label>{"Code shown by your CLI"}<input autocomplete="off" value={(*code).clone()} disabled={*busy||matches!(&*result,Some(Ok(())))} oninput={{let code=code.clone();Callback::from(move |event:InputEvent|code.set(event.target_unchecked_into::<HtmlInputElement>().value()))}} /></label>
        <label class="secrets-check"><input type="checkbox" checked={*confirmed} disabled={*busy} onchange={{let confirmed=confirmed.clone();Callback::from(move |event:Event|confirmed.set(event.target_unchecked_into::<HtmlInputElement>().checked()))}} />{"I started this request and trust this computer to access my secrets."}</label>
        <div class="secrets-actions"><button disabled={*busy||!*verified||!*confirmed||matches!(&*result,Some(Ok(())))} onclick={action(true)}>{"Approve CLI"}</button><button disabled={*busy||matches!(&*result,Some(Ok(())))} onclick={action(false)}>{"Deny"}</button></div>
        if let Some(status)=&*result {{match status{Ok(())=>html!{<p role="status">{"Request handled. Return to your CLI for its result."}</p>},Err(error)=>html!{<><p role="alert">{error}</p><p>{"Verify your account above, then try the action again."}</p></>}}}}
        <Link<Route> to={Route::Secrets}>{"Back to secrets"}</Link<Route>>
    </main>}
}
