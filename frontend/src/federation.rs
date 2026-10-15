//! Device-local connections. The home backend receives metadata only.
use crate::{
    linked_credentials::LinkedCredentials,
    portal_client::{ConnId, PortalClient},
    utils::{self, On401},
};
use gloo_net::http::Request;
use shared::api::{
    FederationDiscoveryResponse, LinkedClientGrant, MeResponse, PortalConnectionsResponse,
    AUTH_MODE_LINKED_DEVICE_FLOW, AUTH_MODE_WS_TICKET, CAP_LINKED_CLIENT_ENROLLMENT, CAP_WS_TICKET,
    FEDERATION_DISCOVERY_PATH, FEDERATION_PROTOCOL,
};
use std::rc::Rc;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{RequestCredentials, RequestRedirect};
use yew::prelude::*;

#[derive(Clone, PartialEq)]
pub struct Connection {
    pub id: ConnId,
    pub origin: String,
    pub user_id: Uuid,
    pub email: String,
    pub instance_id: Uuid,
    pub synced_id: Option<Uuid>,
    pub client: Option<PortalClient>,
    pub status: String,
    pub live_status: Option<String>,
}

impl Connection {
    pub fn display_status(&self) -> String {
        match &self.live_status {
            Some(live) => format!("{} · {live}", self.status),
            None => self.status.clone(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct FederationState {
    pub home: PortalClient,
    pub rows: Vec<Connection>,
    pub loading: bool,
    next_id: u32,
}
impl Default for FederationState {
    fn default() -> Self {
        Self {
            home: PortalClient::home(),
            rows: vec![],
            loading: true,
            next_id: 1,
        }
    }
}

pub enum FederationAction {
    Initialized {
        user: Option<Uuid>,
        rows: Vec<Connection>,
    },
    Add {
        origin: String,
        instance: Uuid,
        grant: Box<LinkedClientGrant>,
        token: String,
        expiry: String,
        synced_id: Option<Uuid>,
    },
    Remove(ConnId),
    Deactivate(ConnId, String),
    Status(ConnId, String),
    LiveStatus(ConnId, Option<String>),
}

impl Reducible for FederationState {
    type Action = FederationAction;
    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut state = (*self).clone();
        match action {
            FederationAction::Initialized { user, mut rows } => {
                state.home = user
                    .map(PortalClient::home_for)
                    .unwrap_or_else(PortalClient::home);
                for row in &mut rows {
                    row.id = ConnId::linked(state.next_id);
                    state.next_id += 1;
                }
                state.rows = rows;
                state.loading = false;
            }
            FederationAction::Add {
                origin,
                instance,
                grant,
                token,
                expiry,
                synced_id,
            } => {
                let existing = state
                    .rows
                    .iter()
                    .position(|r| r.origin == origin && r.user_id == grant.user_id);
                // Re-enrollment gets a fresh connection id, so no old view,
                // pending action or socket can inherit replacement authority.
                let id = ConnId::linked(state.next_id);
                state.next_id += 1;
                let user_id = grant.user_id;
                let email = grant.user_email.clone();
                let result = LinkedCredentials::new(*grant, instance, token, &expiry)
                    .map_err(|e| e.to_string())
                    .and_then(|c| PortalClient::linked(&origin, id, c));
                let (client, status) = match result {
                    Ok(c) => (Some(c), "Connected".into()),
                    Err(e) => (None, e),
                };
                let row = Connection {
                    id,
                    origin,
                    user_id,
                    email,
                    instance_id: instance,
                    synced_id,
                    client,
                    status,
                    live_status: None,
                };
                if let Some(index) = existing {
                    state.rows[index] = row;
                } else {
                    state.rows.push(row);
                }
            }
            FederationAction::Deactivate(id, status) => {
                if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                    row.client = None;
                    row.status = status;
                    row.live_status = None;
                }
            }
            FederationAction::Remove(id) => state.rows.retain(|row| row.id != id),
            FederationAction::LiveStatus(id, status) => {
                if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                    row.live_status = status;
                }
            }
            FederationAction::Status(id, status) => {
                if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                    row.status = status;
                }
            }
        }
        state.into()
    }
}

pub type Federation = UseReducerHandle<FederationState>;

/// Probe without credentials, before offering consent or using stored tokens.
pub async fn discover(origin: &str) -> Result<FederationDiscoveryResponse, String> {
    let origin = shared::urls::canonical_origin(origin).map_err(|e| e.to_string())?;
    let response = Request::get(&format!("{origin}{FEDERATION_DISCOVERY_PATH}"))
        .credentials(RequestCredentials::Omit)
        .redirect(RequestRedirect::Error)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!("Discovery returned HTTP {}", response.status()));
    }
    let data: FederationDiscoveryResponse = response.json().await.map_err(|e| e.to_string())?;
    if data.protocol.highest_common(&FEDERATION_PROTOCOL).is_none() {
        return Err("This portal speaks an incompatible federation protocol".into());
    }
    if ![CAP_LINKED_CLIENT_ENROLLMENT, CAP_WS_TICKET]
        .iter()
        .all(|cap| data.capabilities.iter().any(|c| c == cap))
    {
        return Err("This portal has not enabled the required linking capabilities".into());
    }
    if ![AUTH_MODE_LINKED_DEVICE_FLOW, AUTH_MODE_WS_TICKET]
        .iter()
        .all(|mode| data.auth_modes.iter().any(|m| m == mode))
    {
        return Err("This portal does not support the required authentication modes".into());
    }
    Ok(data)
}

#[derive(Properties, PartialEq)]
pub struct ProviderProps {
    pub children: Children,
}

#[function_component(FederationProvider)]
pub fn provider(props: &ProviderProps) -> Html {
    let state = use_reducer(FederationState::default);
    let bootstrap_error = use_state(String::new);
    let retry = use_state(|| 0u64);
    {
        let state = state.clone();
        let bootstrap_error = bootstrap_error.clone();
        use_effect_with(*retry, move |_| {
            bootstrap_error.set(String::new());
            let cancelled = Rc::new(std::cell::Cell::new(false));
            let guard = cancelled.clone();
            spawn_local(async move {
                // An unavailable identity endpoint is not an anonymous
                // identity. Do not mount a cookie-authorized dashboard without
                // the account binding after a transient bootstrap failure.
                let me = match utils::fetch_json::<MeResponse>("/api/auth/me", On401::Ignore).await
                {
                    Ok(me) => Some(me),
                    Err(utils::FetchError::Status(401)) => None,
                    Err(error) => {
                        if !guard.get() {
                            bootstrap_error.set(format!("Cannot load account: {error}"));
                        }
                        return;
                    }
                };
                let mut rows = Vec::new();
                if let Some(me) = &me {
                    let home = PortalClient::home_for(me.id);
                    if let Ok(list) = home
                        .fetch_json::<PortalConnectionsResponse>(
                            "/api/portal-connections",
                            On401::Ignore,
                        )
                        .await
                    {
                        rows = list
                            .connections
                            .into_iter()
                            .map(|c| Connection {
                                id: ConnId::HOME,
                                origin: c.portal_origin,
                                user_id: c.remote_user_id,
                                email: c.remote_email,
                                instance_id: c.instance_id,
                                synced_id: Some(c.id),
                                client: None,
                                status: "Authorize this device to connect".into(),
                                live_status: None,
                            })
                            .collect();
                    }
                }
                if !guard.get() {
                    state.dispatch(FederationAction::Initialized {
                        user: me.map(|m| m.id),
                        rows,
                    });
                }
            });
            move || cancelled.set(true)
        });
    }
    // A cookie switch in another tab invalidates this view's presentation.
    // X-Portal-Account independently protects requests during the race.
    {
        let account = state.home.account_id();
        use_effect_with(account, move |account| {
            let account = *account;
            let listener = web_sys::window().map(|window| {
                gloo::events::EventListener::new(&window, "focus", move |_| {
                    if let Some(account) = account {
                        spawn_local(async move {
                            let changed =
                                utils::fetch_json::<MeResponse>("/api/auth/me", On401::Ignore)
                                    .await
                                    .map(|me| me.id != account)
                                    .unwrap_or(false);
                            if changed {
                                if let Some(w) = web_sys::window() {
                                    let _ = w.location().reload();
                                }
                            }
                        });
                    }
                })
            });
            move || drop(listener)
        });
    }
    if state.loading {
        let retry_load = {
            let retry = retry.clone();
            Callback::from(move |_| retry.set(*retry + 1))
        };
        return html! { <div class="loading">
            if bootstrap_error.is_empty() { {"Loading account…"} } else {
                <p role="alert">{&*bootstrap_error}</p>
                <button onclick={retry_load}>{"Retry"}</button>
            }
        </div> };
    }
    let home = state.home.clone();
    html! { <ContextProvider<Federation> context={state}>
        <ContextProvider<PortalClient> context={home}>{props.children.clone()}</ContextProvider<PortalClient>>
    </ContextProvider<Federation>> }
}
