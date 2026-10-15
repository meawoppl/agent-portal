//! Native administrator policy controls. A failed load never becomes an
//! implicit Off draft: only a policy read from the server can be edited.
use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::api::{FederationPolicy, TokenScope};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[function_component(AdminFederationTab)]
pub fn admin_federation_tab() -> Html {
    let policy = use_state(|| None::<FederationPolicy>);
    let error = use_state(|| None::<String>);
    let reload = use_state(|| 0u32);
    {
        let policy = policy.clone();
        let error = error.clone();
        use_effect_with(*reload, move |_| {
            policy.set(None);
            error.set(None);
            spawn_local(async move {
                match utils::fetch_json::<FederationPolicy>(
                    "/api/admin/federation-policy",
                    On401::Ignore,
                )
                .await
                {
                    Ok(value) => policy.set(Some(value)),
                    Err(e) => error.set(Some(format!("Could not load federation policy: {e:?}"))),
                }
            });
            || ()
        });
    }
    let retry = {
        let reload = reload.clone();
        Callback::from(move |_| reload.set(*reload + 1))
    };
    html! {
        <section class="section-stack admin-federation">
            <div class="section-header">
                <h2>{"Linked portals"}</h2>
                <p class="section-description">{"Choose which portals may request access to accounts here. Each user must still approve the requesting portal and its permissions."}</p>
            </div>
            if let Some(value) = (*policy).clone() {
                <FederationPolicyEditor initial={value} />
            } else if let Some(message) = &*error {
                <p role="alert">{message}</p>
                <button class="create-button" onclick={retry}>{"Retry"}</button>
            } else {
                <p role="status">{"Loading policy…"}</p>
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct EditorProps {
    initial: FederationPolicy,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Off,
    Allowlist,
    Any,
}

fn make_policy(mode: Mode, scope: TokenScope, text: &str) -> Result<FederationPolicy, String> {
    match mode {
        Mode::Off => Ok(FederationPolicy::Off),
        Mode::Any => Ok(FederationPolicy::Any { max_scope: scope }),
        Mode::Allowlist => {
            let mut origins = Vec::new();
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let origin = shared::urls::canonical_origin(line).map_err(|_| {
                    format!("Invalid portal origin: {line}. Use https://host, without a path.")
                })?;
                if !origins.contains(&origin) {
                    origins.push(origin);
                }
            }
            if origins.is_empty() || origins.len() > 64 {
                return Err("Enter between 1 and 64 portal origins, one per line.".into());
            }
            Ok(FederationPolicy::Allowlist {
                origins,
                max_scope: scope,
            })
        }
    }
}

#[function_component(FederationPolicyEditor)]
fn editor(props: &EditorProps) -> Html {
    // This component owns the editable draft; parent fetch state never
    // round-trips keystrokes or replaces the textarea while typing.
    let (initial_mode, initial_scope, initial_origins) = match &props.initial {
        FederationPolicy::Off => (Mode::Off, TokenScope::Read, String::new()),
        FederationPolicy::Any { max_scope } => (Mode::Any, *max_scope, String::new()),
        FederationPolicy::Allowlist { origins, max_scope } => {
            (Mode::Allowlist, *max_scope, origins.join("\n"))
        }
    };
    let mode = use_state(|| initial_mode);
    let scope = use_state(|| initial_scope);
    let origins = use_state(|| initial_origins);
    let saved = use_state(|| props.initial.clone());
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let notice = use_state(|| None::<String>);
    let on_mode = {
        let mode = mode.clone();
        let notice = notice.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            match input.value().as_str() {
                "off" => mode.set(Mode::Off),
                "allowlist" => mode.set(Mode::Allowlist),
                "any" => mode.set(Mode::Any),
                _ => {}
            }
            notice.set(None);
        })
    };
    let on_scope = {
        let scope = scope.clone();
        let notice = notice.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            if let Some(value) = TokenScope::parse(&input.value()) {
                scope.set(value);
            }
            notice.set(None);
        })
    };
    let on_origins = {
        let origins = origins.clone();
        let notice = notice.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlTextAreaElement = e.target_unchecked_into();
            origins.set(input.value());
            notice.set(None);
        })
    };
    let on_save = {
        let (mode, scope, origins, saved, busy, error, notice) = (
            mode.clone(),
            scope.clone(),
            origins.clone(),
            saved.clone(),
            busy.clone(),
            error.clone(),
            notice.clone(),
        );
        Callback::from(move |_| {
            if *busy {
                return;
            }
            let value = match make_policy(*mode, *scope, &origins) {
                Ok(value) => value,
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            };
            let (saved, busy, error, notice, origins) = (
                saved.clone(),
                busy.clone(),
                error.clone(),
                notice.clone(),
                origins.clone(),
            );
            busy.set(true);
            error.set(None);
            notice.set(None);
            spawn_local(async move {
                match utils::send_json(
                    Request::put(&utils::api_url("/api/admin/federation-policy")),
                    &value,
                )
                .await
                {
                    Ok(response) if response.ok() => match response.json::<FederationPolicy>().await
                    {
                        Ok(value) => {
                            if let FederationPolicy::Allowlist {
                                origins: canonical, ..
                            } = &value
                            {
                                origins.set(canonical.join("\n"));
                            }
                            saved.set(value);
                            notice.set(Some("Policy saved.".into()));
                        }
                        Err(_) => error.set(Some(
                            "The response could not be read. Reload to confirm the saved policy."
                                .into(),
                        )),
                    },
                    Ok(response) => error.set(Some(utils::error_body(response).await)),
                    Err(e) => error.set(Some(format!("Could not save policy: {e}"))),
                }
                busy.set(false);
            });
        })
    };
    let unchanged = make_policy(*mode, *scope, &origins).is_ok_and(|p| p == *saved);
    html! {
        <div class="section-stack">
            <fieldset disabled={*busy}>
                <label for="federation-mode">{"Access policy"}</label>
                <select id="federation-mode" onchange={on_mode}>
                    <option value="off" selected={*mode == Mode::Off}>{"Off"}</option>
                    <option value="allowlist" selected={*mode == Mode::Allowlist}>{"Only listed portals"}</option>
                    <option value="any" selected={*mode == Mode::Any}>{"Any portal, with user approval"}</option>
                </select>
                if *mode == Mode::Allowlist {
                    <label for="federation-origins">{"Allowed portal origins (one per line)"}</label>
                    <textarea id="federation-origins" rows="5" value={(*origins).clone()} oninput={on_origins} placeholder="https://portal.example.org" />
                }
                if *mode != Mode::Off {
                    <label for="federation-scope">{"Maximum permission users may approve"}</label>
                    <select id="federation-scope" onchange={on_scope}>
                        <option value="read" selected={*scope == TokenScope::Read}>{"Read only"}</option>
                        <option value="write" selected={*scope == TokenScope::Write}>{"Read and send commands"}</option>
                    </select>
                    <p class="section-description">{"Session permissions still apply. A viewer cannot become an editor through a linked portal."}</p>
                } else {
                    <p class="section-description">{"Off blocks new links and existing linked access. It does not delete grants; unexpired grants may work again if the policy is re-enabled."}</p>
                }
                <button class="create-button" disabled={*busy || unchanged} onclick={on_save}>{if *busy {"Saving…"} else {"Save policy"}}</button>
            </fieldset>
            if let Some(message) = &*error { <p role="alert">{message}</p> }
            if let Some(message) = &*notice { <p role="status">{message}</p> }
        </div>
    }
}
