//! Inline verification. Only a server-confirmed success enables the pending action.
use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::api::SecretVerificationStatus;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    #[prop_or_default]
    pub on_verified: Callback<bool>,
}
#[derive(Clone, PartialEq)]
enum CheckState {
    Loading,
    Ready,
    Checking,
    Verified,
    Failed(String),
}
#[function_component(SecretVerification)]
pub fn secret_verification(props: &Props) -> Html {
    let state = use_state(|| CheckState::Loading);
    let busy = use_mut_ref(|| false);
    {
        let state = state.clone();
        let on_verified = props.on_verified.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match utils::fetch_json::<SecretVerificationStatus>(
                    "/api/secrets/verification",
                    On401::Ignore,
                )
                .await
                {
                    Ok(status) if status.verified => {
                        state.set(CheckState::Verified);
                        on_verified.emit(true);
                    }
                    Ok(_) => state.set(CheckState::Ready),
                    Err(_) => state.set(CheckState::Failed(
                        "Could not check your account. Try again.".into(),
                    )),
                }
            });
            || ()
        });
    }
    let check = {
        let state = state.clone();
        let busy = busy.clone();
        let on_verified = props.on_verified.clone();
        Callback::from(move |_| {
            if *busy.borrow() || *state == CheckState::Verified {
                return;
            }
            *busy.borrow_mut() = true;
            state.set(CheckState::Checking);
            let (state, busy, on_verified) = (state.clone(), busy.clone(), on_verified.clone());
            spawn_local(async move {
                let request = async {
                    let response =
                        Request::post(&utils::api_url("/api/secrets/verification/check"))
                            .header("x-portal-secrets", "1")
                            .send()
                            .await
                            .map_err(|_| "Could not verify your account. Try again.")?;
                    if response.status() == 401 {
                        return Err(
                            "Your Portal login has expired. Sign in again before approving access.",
                        );
                    }
                    if !response.ok() {
                        return Err("Account verification did not succeed. Try again or check your account access.");
                    }
                    let status = response
                        .json::<SecretVerificationStatus>()
                        .await
                        .map_err(|_| "Invalid verification response. Try again.")?;
                    if !status.verified {
                        return Err("Account verification did not succeed. Try again.");
                    }
                    Ok(())
                };
                let timeout = gloo::timers::future::TimeoutFuture::new(15_000);
                futures_util::pin_mut!(request, timeout);
                let result = match futures_util::future::select(request, timeout).await {
                    futures_util::future::Either::Left((result, _)) => result,
                    futures_util::future::Either::Right(_) => {
                        Err("Verification timed out. Try again.")
                    }
                };
                *busy.borrow_mut() = false;
                match result {
                    Ok(()) => {
                        state.set(CheckState::Verified);
                        on_verified.emit(true);
                    }
                    Err(error) => state.set(CheckState::Failed(error.into())),
                }
            });
        })
    };
    let working = matches!(*state, CheckState::Loading | CheckState::Checking);
    html! {<section class={classes!("secrets-verification", (*state == CheckState::Verified).then_some("is-verified"))} aria-label="Verify your account" aria-busy={working.to_string()}>
        {match &*state {
            CheckState::Verified => html!{<p role="status">{"Portal session verified"}</p>},
            CheckState::Loading | CheckState::Checking => html!{<p role="status"><span class="secret-verification-spinner" aria-hidden="true"/>{"Checking your Portal session…"}</p>},
            _ => html!{<>
                if let CheckState::Failed(message) = &*state {<p role="alert">{message}</p>}
                else {<p>{"Check your Portal session before approving secret access."}</p>}
                <button type="button" class="secret-button secret-button--primary" onclick={check}>{if matches!(*state,CheckState::Failed(_)) {"Try again"} else {"Verify Portal session"}}</button>
            </>},
        }}
    </section>}
}
