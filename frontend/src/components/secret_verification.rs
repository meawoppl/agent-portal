//! Explicit account check; returning never approves a pending action automatically.
use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::api::{SecretVerificationRedirect, SecretVerificationStart, SecretVerificationStatus};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    /// Preserve a manually entered enrollment code across the provider redirect.
    #[prop_or_default]
    pub return_to: Option<String>,
    #[prop_or_default]
    pub on_verified: Callback<bool>,
}
#[function_component(SecretVerification)]
pub fn secret_verification(props: &Props) -> Html {
    let status = use_state(|| None::<SecretVerificationStatus>);
    let error = use_state(|| {
        web_sys::window().and_then(|w| w.location().search().ok())
            .filter(|q| q.contains("secret_verification=failed"))
            .map(|_| "Verification was cancelled or used a different account. Try again with your Portal account.".to_string())
    });
    let busy = use_state(|| false);
    {
        let status = status.clone();
        let error = error.clone();
        let on_verified = props.on_verified.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match utils::fetch_json::<SecretVerificationStatus>(
                    "/api/secrets/verification",
                    On401::Ignore,
                )
                .await
                {
                    Ok(value) => {
                        if value.verified {
                            error.set(None);
                        }
                        on_verified.emit(value.verified);
                        status.set(Some(value));
                    }
                    Err(_) => error.set(Some(
                        "Could not check account verification. Refresh to retry.".into(),
                    )),
                }
            });
            || ()
        });
    }
    let start = |provider: String| {
        let error = error.clone();
        let busy = busy.clone();
        let return_to = props.return_to.clone();
        Callback::from(move |_| {
            if *busy {
                return;
            }
            busy.set(true);
            error.set(None);
            let busy = busy.clone();
            let error = error.clone();
            let provider = provider.clone();
            let return_to = return_to.clone();
            spawn_local(async move {
                let result: Result<(), String> = async {
                    let window = web_sys::window().ok_or("Browser unavailable")?;
                    let location = window.location();
                    let return_to = match return_to {
                        Some(value) => value,
                        None => format!(
                            "{}{}{}",
                            location.pathname().map_err(|_| "Page unavailable")?,
                            location.search().map_err(|_| "Page unavailable")?,
                            location.hash().map_err(|_| "Page unavailable")?
                        ),
                    };
                    let request = Request::post(&utils::api_url("/api/secrets/verification/start"))
                        .header("x-portal-secrets", "1");
                    let response = utils::send_json(
                        request,
                        &SecretVerificationStart {
                            provider,
                            return_to,
                        },
                    )
                    .await
                    .map_err(|_| "Could not start verification")?;
                    if !response.ok() {
                        return Err(
                            "Verification unavailable for this account. Refresh and try again."
                                .into(),
                        );
                    }
                    let redirect = response
                        .json::<SecretVerificationRedirect>()
                        .await
                        .map_err(|_| "Invalid verification response")?;
                    location
                        .set_href(&redirect.url)
                        .map_err(|_| "Could not open account verification")?;
                    Ok(())
                }
                .await;
                if let Err(message) = result {
                    error.set(Some(message));
                    busy.set(false);
                }
            });
        })
    };
    html! {<section class={classes!("secrets-verification", status.as_ref().is_some_and(|s|s.verified).then_some("is-verified"))} aria-label="Verify your account">
        if let Some(status)=&*status {
            if status.verified {<p role="status">{"Account verified"}</p>}
            else {
                <p>{"Verify your account before approving access or changing protected secret settings. You will return here to finish the action."}</p>
                {for status.providers.iter().map(|provider| {
                    let label=if provider=="google" {"Verify with Google"} else {"Verify with GitHub"};
                    html!{<button type="button" class="secret-button secret-button--primary" disabled={*busy} onclick={start(provider.clone())}>{label}</button>}
                })}
                if status.providers.is_empty() {<p>{"No linked login provider is configured for verification."}</p>}
            }
        } else if error.is_none() {<p>{"Checking account verification…"}</p>}
        if let Some(error)=&*error {<p role="alert">{error}</p>}
    </section>}
}
