//! Profile settings: the display nickname (#1485).
//!
//! A cosmetic label shown in message attribution instead of the account name;
//! auth, membership and admin views keep the real name/email. Prefilled from
//! `/api/auth/me` and saved via `PUT /api/settings/profile`.

use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::api::{MeResponse, UpdateProfileRequest};
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[function_component(ProfilePanel)]
pub fn profile_panel() -> Html {
    // The edited draft; seeded from the server once `/api/auth/me` lands.
    let nickname = use_state(String::new);
    // The account's real name/email, shown so the user knows the fallback.
    let name = use_state(|| None::<String>);
    let email = use_state(String::new);
    let loading = use_state(|| true);
    let saving = use_state(|| false);
    let feedback = use_state(|| None::<&'static str>);

    {
        let nickname = nickname.clone();
        let name = name.clone();
        let email = email.clone();
        let loading = loading.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(me) = utils::fetch_json::<MeResponse>("/api/auth/me", On401::Ignore).await
                {
                    nickname.set(me.nickname.unwrap_or_default());
                    name.set(me.name);
                    email.set(me.email);
                }
                loading.set(false);
            });
        });
    }

    let on_input = {
        let nickname = nickname.clone();
        let feedback = feedback.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            nickname.set(input.value());
            // A pending edit invalidates the last "Saved" note.
            feedback.set(None);
        })
    };

    let on_save = {
        let nickname = nickname.clone();
        let saving = saving.clone();
        let feedback = feedback.clone();
        Callback::from(move |_: MouseEvent| {
            let trimmed = (*nickname).trim().to_string();
            let body = UpdateProfileRequest {
                // Blank clears the nickname; send `None` so the server stores NULL.
                nickname: (!trimmed.is_empty()).then(|| trimmed.clone()),
            };
            let saving = saving.clone();
            let feedback = feedback.clone();
            let nickname = nickname.clone();
            saving.set(true);
            spawn_local(async move {
                let ok = utils::send_json(
                    Request::put(&utils::api_url("/api/settings/profile")),
                    &body,
                )
                .await
                .map(|r| r.ok())
                .unwrap_or(false);
                // Reflect the normalized value locally so the field matches
                // what the server stored.
                nickname.set(trimmed);
                saving.set(false);
                feedback.set(Some(if ok {
                    "Saved"
                } else {
                    "Couldn't save — try again"
                }));
            });
        })
    };

    let fallback = (*name)
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| (*email).clone());

    html! {
        <section class="profile-section">
            <div class="section-header">
                <h2>{ "Profile" }</h2>
                <p class="section-description">
                    { "How you appear in message attribution. Cosmetic only — \
                       your account name and email are unchanged." }
                </p>
            </div>

            if *loading {
                <p class="setting-description">{ "Loading…" }</p>
            } else {
                <div class="profile-setting">
                    <h3>{ "Display nickname" }</h3>
                    <p class="setting-description">
                        { format!("Leave blank to show \u{201c}{fallback}\u{201d}.") }
                    </p>
                    <input
                        type="text"
                        class="profile-nickname-input"
                        maxlength="64"
                        placeholder={fallback.clone()}
                        value={(*nickname).clone()}
                        oninput={on_input}
                    />
                    <div class="profile-save-row">
                        <button
                            class="create-button"
                            onclick={on_save}
                            disabled={*saving}
                        >
                            { if *saving { "Saving…" } else { "Save" } }
                        </button>
                        if let Some(msg) = *feedback {
                            <span class="save-feedback">{ msg }</span>
                        }
                    </div>
                </div>
            }
        </section>
    }
}
