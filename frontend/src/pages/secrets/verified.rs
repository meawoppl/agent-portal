//! Popup landing page. This signal is not an authorization proof: the original
//! page must re-fetch authenticated verification status before enabling approval.
use yew::prelude::*;
#[function_component(SecretVerifiedPage)]
pub fn secret_verified_page() -> Html {
    use_effect_with((), |_| {
        if let Some(window) = web_sys::window() {
            if let Ok(search) = window.location().search() {
                if let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search) {
                    if let Some(name) = params
                        .get("channel")
                        .filter(|s| s.starts_with("portal-secret-verification-") && s.len() < 100)
                    {
                        if let Ok(channel) = web_sys::BroadcastChannel::new(&name) {
                            let _ =
                                channel.post_message(&wasm_bindgen::JsValue::from_str("complete"));
                            channel.close();
                        }
                    }
                }
            }
            let _ = window.close();
        }
        || ()
    });
    html! { <main class="secrets-page secrets-authorization"><h1>{"Return to Portal"}</h1><p>{"The verification window can now be closed. Your original page will check the result. No access has been approved."}</p><button type="button" class="secret-button" onclick={Callback::from(|_| {if let Some(w)=web_sys::window() {let _=w.close();}})}>{"Close window"}</button></main> }
}
