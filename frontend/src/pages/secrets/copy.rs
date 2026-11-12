//! Plaintext is transient: never stored in component state, markup or diagnostics.
use super::api;
use shared::api::{SecretReadRequest, SecretReadResponse};
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local, JsFuture};
use yew::prelude::*;

// Start clipboard.write synchronously during the click. Supplying a promised
// Blob preserves Safari's user-gesture requirement while the gated fetch runs.
#[wasm_bindgen(inline_js = r#"
export function secretClipboardAvailable() {
    return !!(globalThis.isSecureContext && navigator.clipboard?.write && globalThis.ClipboardItem);
}
export function writeSecretClipboard(value) {
    return navigator.clipboard.write([new ClipboardItem({'text/plain': value})]);
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = secretClipboardAvailable)]
    fn available() -> bool;
    #[wasm_bindgen(catch, js_name = writeSecretClipboard)]
    fn write(value: &js_sys::Promise) -> Result<js_sys::Promise, JsValue>;
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub id: Uuid,
    pub version: i32,
    pub verified: bool,
    pub disabled: bool,
}
#[derive(Clone, PartialEq)]
enum Status {
    Ready,
    Copying,
    Copied,
    Failed,
}
#[function_component(SecretCopy)]
pub fn secret_copy(props: &Props) -> Html {
    let status = use_state(|| Status::Ready);
    let busy = use_mut_ref(|| false);
    let onclick = {
        let status = status.clone();
        let busy = busy.clone();
        let (id, version, allowed) = (props.id, props.version, props.verified && !props.disabled);
        Callback::from(move |_| {
            if !allowed || *busy.borrow() {
                return;
            }
            if !available() {
                status.set(Status::Failed);
                return;
            }
            *busy.borrow_mut() = true;
            status.set(Status::Copying);
            let value = future_to_promise(async move {
                let result = async {
                    let response = api::mutate(
                        "POST",
                        &format!("/api/secrets/{id}/copy"),
                        &SecretReadRequest {
                            version: Some(version),
                            for_execution: false,
                        },
                    )
                    .await
                    .map_err(|_| ())?;
                    let value = response
                        .json::<SecretReadResponse>()
                        .await
                        .map_err(|_| ())?;
                    if value.version != version {
                        return Err(());
                    }
                    let parts = js_sys::Array::new();
                    parts.push(&JsValue::from_str(&value.value.0));
                    let options = web_sys::BlobPropertyBag::new();
                    options.set_type("text/plain");
                    web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)
                        .map_err(|_| ())
                }
                .await;
                result
                    .map(JsValue::from)
                    .map_err(|_| JsValue::from_str("Secret copy unavailable"))
            });
            let fetched = JsFuture::from(value.clone());
            let writing = write(&value);
            let (status, busy) = (status.clone(), busy.clone());
            spawn_local(async move {
                // Observe both promises even if the browser rejects clipboard
                // access immediately, so no rejected plaintext-fetch promise leaks.
                let result = match writing {
                    Ok(promise) => JsFuture::from(promise).await.map(|_| ()),
                    Err(error) => Err(error),
                };
                let fetched = fetched.await;
                *busy.borrow_mut() = false;
                status.set(if result.is_ok() && fetched.is_ok() {
                    Status::Copied
                } else {
                    Status::Failed
                });
            });
        })
    };
    let copying = *status == Status::Copying;
    html! {<div class="secret-copy">
        <button type="button" disabled={!props.verified || props.disabled || copying} {onclick}>
            {if copying {"Copying…"} else {"Copy secret"}}
        </button>
        if !props.verified {<small>{"Verify your Portal session above to copy."}</small>}
        {match *status {
            Status::Copied => html!{<span role="status">{"Copied to clipboard"}</span>},
            Status::Copying => html!{<span role="status"><span class="secret-verification-spinner" aria-hidden="true"/>{"Copying to clipboard…"}</span>},
            Status::Failed => html!{<span role="alert">{"Could not copy. Check account verification, secret access, and browser clipboard permission, then retry."}</span>},
            Status::Ready => Html::default(),
        }}
    </div>}
}
