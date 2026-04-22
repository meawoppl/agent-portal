use crate::utils;
use gloo_net::http::Request;
use shared::{
    CreateProxyTokenRequest, CreateProxyTokenResponse, ProxyTokenInfo, ProxyTokenListResponse,
    RenewProxyTokenRequest,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Calculate days until expiration from ISO date string
fn days_until_expiration(expires_at: &str) -> Option<i64> {
    let now = js_sys::Date::now();
    let expires = js_sys::Date::parse(expires_at);
    if expires.is_nan() {
        return None;
    }
    let diff_ms = expires - now;
    let diff_days = (diff_ms / (1000.0 * 60.0 * 60.0 * 24.0)).floor() as i64;
    Some(diff_days)
}

/// Count tokens expiring within 7 days (used by the tab badge in the parent)
pub fn count_expiring_tokens(tokens: &[ProxyTokenInfo]) -> usize {
    tokens
        .iter()
        .filter(|t| !t.revoked)
        .filter(|t| {
            days_until_expiration(&t.expires_at)
                .map(|d| (0..=7).contains(&d))
                .unwrap_or(false)
        })
        .count()
}

/// Fetch tokens from API, returning the list
pub async fn fetch_tokens_from_api() -> Option<Vec<ProxyTokenInfo>> {
    let api_endpoint = utils::api_url("/api/proxy-tokens");
    match Request::get(&api_endpoint).send().await {
        Ok(response) => {
            if response.status() == 401 {
                if let Some(window) = web_sys::window() {
                    let _ = window.location().set_href("/api/auth/logout");
                }
                return None;
            }
            if let Ok(data) = response.json::<ProxyTokenListResponse>().await {
                Some(data.tokens)
            } else {
                None
            }
        }
        Err(e) => {
            log::error!("Failed to fetch tokens: {:?}", e);
            None
        }
    }
}

#[derive(Properties, PartialEq)]
struct TokenRowProps {
    token: ProxyTokenInfo,
    on_revoke: Callback<Uuid>,
    on_renew: Callback<Uuid>,
}

#[function_component(TokenRow)]
fn token_row(props: &TokenRowProps) -> Html {
    let token = &props.token;
    let on_revoke = props.on_revoke.clone();
    let on_renew = props.on_renew.clone();
    let token_id = token.id;

    let days_left = days_until_expiration(&token.expires_at);
    let is_expired = days_left.map(|d| d < 0).unwrap_or(false);
    let is_expiring_soon = days_left.map(|d| (0..=7).contains(&d)).unwrap_or(false);

    let status_class = if token.revoked {
        "token-status revoked"
    } else if is_expired {
        "token-status expired"
    } else if is_expiring_soon {
        "token-status expiring-soon"
    } else {
        "token-status active"
    };

    let status_text = if token.revoked {
        "Revoked".to_string()
    } else if is_expired {
        "Expired".to_string()
    } else if let Some(days) = days_left {
        if days == 0 {
            "Expires today".to_string()
        } else if days == 1 {
            "Expires tomorrow".to_string()
        } else if days <= 7 {
            format!("Expires in {} days", days)
        } else {
            "Active".to_string()
        }
    } else {
        "Active".to_string()
    };

    let on_revoke_click = Callback::from(move |_| {
        on_revoke.emit(token_id);
    });

    let on_renew_click = {
        let on_renew = on_renew.clone();
        Callback::from(move |_| {
            on_renew.emit(token_id);
        })
    };

    html! {
        <tr class={if token.revoked || is_expired { "token-row disabled" } else { "token-row" }}>
            <td class="token-name">{ &token.name }</td>
            <td class="token-created">{ utils::format_timestamp(&token.created_at) }</td>
            <td class="token-last-used">
                { token.last_used_at.as_ref().map(|t| utils::format_timestamp(t)).unwrap_or_else(|| "Never".to_string()) }
            </td>
            <td class="token-expires">{ utils::format_timestamp(&token.expires_at) }</td>
            <td class={status_class}>{ status_text }</td>
            <td class="token-actions">
                if !token.revoked {
                    <button class="renew-button" onclick={on_renew_click}>
                        { "Renew" }
                    </button>
                }
                if !token.revoked && !is_expired {
                    <button class="revoke-button" onclick={on_revoke_click}>
                        { "Revoke" }
                    </button>
                }
            </td>
        </tr>
    }
}

#[derive(Clone, Default)]
struct NewTokenForm {
    name: String,
    expires_in_days: u32,
}

#[derive(Properties, PartialEq)]
pub struct TokensPanelProps {
    pub on_tokens_loaded: Callback<Vec<ProxyTokenInfo>>,
}

#[function_component(TokensPanel)]
pub fn tokens_panel(props: &TokensPanelProps) -> Html {
    let tokens = use_state(Vec::<ProxyTokenInfo>::new);
    let tokens_loading = use_state(|| true);
    let new_token_form = use_state(NewTokenForm::default);
    let created_token = use_state(|| None::<CreateProxyTokenResponse>);
    let show_create_form = use_state(|| false);
    let confirm_action = use_state(|| None::<(String, Callback<MouseEvent>)>);

    let fetch_tokens = {
        let tokens = tokens.clone();
        let tokens_loading = tokens_loading.clone();
        let on_tokens_loaded = props.on_tokens_loaded.clone();

        Callback::from(move |_| {
            let tokens = tokens.clone();
            let tokens_loading = tokens_loading.clone();
            let on_tokens_loaded = on_tokens_loaded.clone();

            spawn_local(async move {
                if let Some(token_list) = fetch_tokens_from_api().await {
                    on_tokens_loaded.emit(token_list.clone());
                    tokens.set(token_list);
                }
                tokens_loading.set(false);
            });
        })
    };

    // Initial fetch
    {
        let fetch_tokens = fetch_tokens.clone();
        use_effect_with((), move |_| {
            fetch_tokens.emit(());
            || ()
        });
    }

    let on_revoke_token = {
        let tokens = tokens.clone();
        let confirm_action = confirm_action.clone();

        Callback::from(move |token_id: Uuid| {
            let tokens = tokens.clone();
            let confirm_action_inner = confirm_action.clone();

            let action = Callback::from(move |_: MouseEvent| {
                let tokens = tokens.clone();
                let confirm_action_inner = confirm_action_inner.clone();

                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/proxy-tokens/{}", token_id));
                    match Request::delete(&api_endpoint).send().await {
                        Ok(response) => {
                            if response.status() == 204 || response.status() == 200 {
                                let mut updated: Vec<ProxyTokenInfo> = (*tokens).to_vec();
                                if let Some(token) = updated.iter_mut().find(|t| t.id == token_id) {
                                    token.revoked = true;
                                }
                                tokens.set(updated);
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to revoke token: {:?}", e);
                        }
                    }
                    confirm_action_inner.set(None);
                });
            });

            confirm_action.set(Some((
                "Revoke this token? Connected proxies will be disconnected.".to_string(),
                action,
            )));
        })
    };

    let renewed_token = use_state(|| None::<CreateProxyTokenResponse>);

    let on_renew_token = {
        let confirm_action = confirm_action.clone();
        let renewed_token = renewed_token.clone();
        let fetch_tokens = fetch_tokens.clone();

        Callback::from(move |token_id: Uuid| {
            let confirm_action_inner = confirm_action.clone();
            let renewed_token = renewed_token.clone();
            let fetch_tokens = fetch_tokens.clone();

            let action = Callback::from(move |_: MouseEvent| {
                let confirm_action_inner = confirm_action_inner.clone();
                let renewed_token = renewed_token.clone();
                let fetch_tokens = fetch_tokens.clone();

                spawn_local(async move {
                    let api_endpoint =
                        utils::api_url(&format!("/api/proxy-tokens/{}/renew", token_id));
                    let request_body = RenewProxyTokenRequest {
                        expires_in_days: 30,
                    };

                    match Request::post(&api_endpoint)
                        .json(&request_body)
                        .unwrap()
                        .send()
                        .await
                    {
                        Ok(response) => {
                            if let Ok(data) = response.json::<CreateProxyTokenResponse>().await {
                                renewed_token.set(Some(data));
                                fetch_tokens.emit(());
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to renew token: {:?}", e);
                        }
                    }
                    confirm_action_inner.set(None);
                });
            });

            confirm_action.set(Some((
                "Renew this token for 30 days? You will need to update your proxy with the new token.".to_string(),
                action,
            )));
        })
    };

    let on_create_token = {
        let new_token_form = new_token_form.clone();
        let created_token = created_token.clone();
        let fetch_tokens = fetch_tokens.clone();

        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let form_data = (*new_token_form).clone();
            let created_token = created_token.clone();
            let fetch_tokens = fetch_tokens.clone();

            if form_data.name.trim().is_empty() {
                return;
            }

            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/proxy-tokens");
                let request_body = CreateProxyTokenRequest {
                    name: form_data.name.trim().to_string(),
                    expires_in_days: if form_data.expires_in_days > 0 {
                        form_data.expires_in_days
                    } else {
                        30
                    },
                };

                match Request::post(&api_endpoint)
                    .json(&request_body)
                    .unwrap()
                    .send()
                    .await
                {
                    Ok(response) => {
                        if let Ok(data) = response.json::<CreateProxyTokenResponse>().await {
                            created_token.set(Some(data));
                            fetch_tokens.emit(());
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to create token: {:?}", e);
                    }
                }
            });
        })
    };

    let on_name_input = {
        let new_token_form = new_token_form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut form = (*new_token_form).clone();
            form.name = input.value();
            new_token_form.set(form);
        })
    };

    let on_days_input = {
        let new_token_form = new_token_form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut form = (*new_token_form).clone();
            form.expires_in_days = input.value().parse().unwrap_or(30);
            new_token_form.set(form);
        })
    };

    let toggle_create_form = {
        let show_create_form = show_create_form.clone();
        let created_token = created_token.clone();
        let new_token_form = new_token_form.clone();
        Callback::from(move |_| {
            if *show_create_form {
                created_token.set(None);
                new_token_form.set(NewTokenForm::default());
            }
            show_create_form.set(!*show_create_form);
        })
    };

    let cancel_confirm = {
        let confirm_action = confirm_action.clone();
        Callback::from(move |_| {
            confirm_action.set(None);
        })
    };

    html! {
        <>
            <section class="tokens-section">
                <div class="section-header">
                    <h2>{ "Proxy Credentials" }</h2>
                    <p class="section-description">
                        { "Manage authentication tokens for your Claude Code proxy connections." }
                    </p>
                    <button class="create-button" onclick={toggle_create_form.clone()}>
                        { if *show_create_form { "Cancel" } else { "+ Create Token" } }
                    </button>
                </div>

                if *show_create_form {
                    <div class="create-token-form">
                        if let Some(token_response) = &*created_token {
                            <div class="token-created-success">
                                <h3>{ "Token Created Successfully" }</h3>
                                <p class="warning">
                                    { "Copy this token now. It will not be shown again!" }
                                </p>
                                <div class="token-display">
                                    <code>{ &token_response.token }</code>
                                </div>
                                <div class="init-url">
                                    <label>{ "Or use this initialization URL:" }</label>
                                    <code>{ &token_response.init_url }</code>
                                </div>
                                <p class="expires-info">
                                    { format!("Expires: {}", utils::format_timestamp(&token_response.expires_at)) }
                                </p>
                                <button onclick={toggle_create_form.clone()}>{ "Done" }</button>
                            </div>
                        } else {
                            <form onsubmit={on_create_token}>
                                <div class="form-group">
                                    <label for="token-name">{ "Token Name" }</label>
                                    <input
                                        type="text"
                                        id="token-name"
                                        placeholder="e.g., My Laptop, Work Machine"
                                        value={new_token_form.name.clone()}
                                        oninput={on_name_input}
                                        required=true
                                    />
                                </div>
                                <div class="form-group">
                                    <label for="token-days">{ "Expires In (days)" }</label>
                                    <input
                                        type="number"
                                        id="token-days"
                                        min="1"
                                        max="365"
                                        value={new_token_form.expires_in_days.to_string()}
                                        oninput={on_days_input}
                                    />
                                </div>
                                <button type="submit" class="submit-button">
                                    { "Create Token" }
                                </button>
                            </form>
                        }
                    </div>
                }

                if *tokens_loading {
                    <div class="loading">
                        <div class="spinner"></div>
                        <p>{ "Loading tokens..." }</p>
                    </div>
                } else if tokens.is_empty() {
                    <div class="empty-state">
                        <p>{ "No tokens found. Create one to connect a proxy." }</p>
                    </div>
                } else {
                    <div class="table-container">
                        <table class="tokens-table">
                            <thead>
                                <tr>
                                    <th>{ "Name" }</th>
                                    <th>{ "Created" }</th>
                                    <th>{ "Last Used" }</th>
                                    <th>{ "Expires" }</th>
                                    <th>{ "Status" }</th>
                                    <th>{ "Actions" }</th>
                                </tr>
                            </thead>
                            <tbody>
                                { for tokens.iter().map(|token| {
                                    html! {
                                        <TokenRow
                                            key={token.id.to_string()}
                                            token={token.clone()}
                                            on_revoke={on_revoke_token.clone()}
                                            on_renew={on_renew_token.clone()}
                                        />
                                    }
                                }) }
                            </tbody>
                        </table>
                    </div>
                    <p class="section-note">
                        { "Credentials expired for more than 7 days are automatically deleted." }
                    </p>
                }
            </section>

            if let Some(token_response) = &*renewed_token {
                <div class="modal-overlay" onclick={{
                    let renewed_token = renewed_token.clone();
                    Callback::from(move |_| renewed_token.set(None))
                }}>
                    <div class="confirm-modal token-renewed-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <div class="token-created-success">
                            <h3>{ "Token Renewed" }</h3>
                            <p class="warning">
                                { "Copy this token now. It will not be shown again!" }
                            </p>
                            <div class="token-display">
                                <code>{ &token_response.token }</code>
                            </div>
                            <div class="init-url">
                                <label>{ "Or use this initialization URL:" }</label>
                                <code>{ &token_response.init_url }</code>
                            </div>
                            <p class="expires-info">
                                { format!("Expires: {}", utils::format_timestamp(&token_response.expires_at)) }
                            </p>
                            <button onclick={{
                                let renewed_token = renewed_token.clone();
                                Callback::from(move |_| renewed_token.set(None))
                            }}>{ "Done" }</button>
                        </div>
                    </div>
                </div>
            }

            if let Some((message, action)) = &*confirm_action {
                <div class="modal-overlay" onclick={cancel_confirm.clone()}>
                    <div class="confirm-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <p>{ message }</p>
                        <div class="confirm-actions">
                            <button class="cancel-button" onclick={cancel_confirm.clone()}>
                                { "Cancel" }
                            </button>
                            <button class="confirm-button" onclick={action.clone()}>
                                { "Confirm" }
                            </button>
                        </div>
                    </div>
                </div>
            }
        </>
    }
}
