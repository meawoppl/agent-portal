//! Account profile and linked login methods (#1535).

use super::profile_panel::ProfilePanel;
use crate::components::{ConfirmModal, ConfirmModalStyle};
use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::api::{LinkedIdentitiesResponse, LinkedIdentity};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

fn provider_name(provider: &str) -> String {
    match provider {
        "google" => "Google".to_string(),
        "github" => "GitHub".to_string(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_else(|| "Login".to_string())
        }
    }
}

#[function_component(AccountPanel)]
pub fn account_panel() -> Html {
    let identities = use_state(|| None::<Vec<LinkedIdentity>>);
    let load_error = use_state(|| None::<String>);
    let pending_unlink = use_state(|| None::<LinkedIdentity>);
    let unlinking = use_state(|| None::<Uuid>);

    {
        let identities = identities.clone();
        let load_error = load_error.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match utils::fetch_json::<LinkedIdentitiesResponse>(
                    "/api/settings/identities",
                    On401::Logout,
                )
                .await
                {
                    Ok(response) => identities.set(Some(response.identities)),
                    Err(error) => load_error.set(Some(error.to_string())),
                }
            });
        });
    }

    let cancel_unlink = {
        let pending_unlink = pending_unlink.clone();
        Callback::from(move |_: MouseEvent| pending_unlink.set(None))
    };
    let confirm_unlink = {
        let identities = identities.clone();
        let pending_unlink = pending_unlink.clone();
        let unlinking = unlinking.clone();
        let load_error = load_error.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(identity) = (*pending_unlink).clone() else {
                return;
            };
            pending_unlink.set(None);
            unlinking.set(Some(identity.id));
            let identities = identities.clone();
            let unlinking = unlinking.clone();
            let load_error = load_error.clone();
            spawn_local(async move {
                let response = Request::delete(&utils::api_url(&format!(
                    "/api/settings/identities/{}",
                    identity.id
                )))
                .send()
                .await;
                match response {
                    Ok(response) if response.ok() => {
                        if let Some(current) = &*identities {
                            identities.set(Some(
                                current
                                    .iter()
                                    .filter(|item| item.id != identity.id)
                                    .cloned()
                                    .collect(),
                            ));
                        }
                        load_error.set(None);
                    }
                    Ok(response) => load_error.set(Some(format!(
                        "Couldn't unlink this login (HTTP {}).",
                        response.status()
                    ))),
                    Err(error) => {
                        load_error.set(Some(format!("Couldn't unlink this login: {error}")))
                    }
                }
                unlinking.set(None);
            });
        })
    };

    html! {
        <div class="section-stack account-settings">
            <ProfilePanel />

            <section class="section-stack linked-logins-section">
                <div class="section-header">
                    <h2>{ "Login methods" }</h2>
                    <p class="section-description">
                        { "External accounts you can use to sign in. Keep at least one login method linked." }
                    </p>
                </div>

                if let Some(error) = &*load_error {
                    <p class="settings-error" role="alert">{ error }</p>
                }

                if let Some(items) = &*identities {
                    <div class="identity-list">
                        { for items.iter().map(|identity| {
                            let identity_for_click = identity.clone();
                            let pending_unlink = pending_unlink.clone();
                            let only_identity = items.len() == 1;
                            let is_unlinking = *unlinking == Some(identity.id);
                            let onclick = Callback::from(move |_: MouseEvent| {
                                pending_unlink.set(Some(identity_for_click.clone()));
                            });
                            html! {
                                <div class="identity-row" key={identity.id.to_string()}>
                                    <span class={classes!("identity-provider", format!("provider-{}", identity.provider))}>
                                        { provider_name(&identity.provider) }
                                    </span>
                                    <div class="identity-details">
                                        <strong>{ identity.email.as_deref().unwrap_or("Linked account") }</strong>
                                        <span>{ "Linked login" }</span>
                                    </div>
                                    <button
                                        type="button"
                                        class="delete-button"
                                        {onclick}
                                        disabled={only_identity || is_unlinking}
                                        title={only_identity.then_some("You cannot unlink your last login method")}
                                    >
                                        { if is_unlinking { "Unlinking…" } else { "Unlink" } }
                                    </button>
                                </div>
                            }
                        }) }
                    </div>
                } else if load_error.is_none() {
                    <p class="setting-description">{ "Loading login methods…" }</p>
                }
            </section>

            if let Some(identity) = &*pending_unlink {
                <ConfirmModal
                    title="Unlink login method"
                    message={format!(
                        "Unlink {}{} from this account?",
                        provider_name(&identity.provider),
                        identity.email.as_ref().map(|email| format!(" ({email})")).unwrap_or_default()
                    )}
                    warning="You will no longer be able to sign in with this identity."
                    confirm_label="Unlink"
                    style={ConfirmModalStyle::Panel}
                    on_confirm={confirm_unlink}
                    on_cancel={cancel_unlink}
                />
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::provider_name;

    #[test]
    fn provider_names_cover_known_and_generic_logins() {
        assert_eq!(provider_name("google"), "Google");
        assert_eq!(provider_name("github"), "GitHub");
        assert_eq!(provider_name("gitlab"), "Gitlab");
    }
}
