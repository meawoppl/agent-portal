//! The portal a piece of UI talks to.
//!
//! Every network access that belongs to a *session* — REST calls, the
//! per-session WebSocket, file pulls, media URLs — resolves against a
//! [`PortalClient`] instead of `window.location`, so the same components can
//! render sessions hosted by a different portal (federated views, see
//! `docs/design/FEDERATION_DISCUSSION.md`). Account-level pages (settings,
//! admin) stay on the home portal through [`crate::utils`].
//!
//! The root of the app provides the home portal as a Yew context; each
//! `SessionView` re-provides the portal its session lives on, so descendants
//! read it with [`use_portal`] (function components) or
//! [`portal_from_context`] (struct components) rather than threading props.

use std::rc::Rc;

use gloo_net::http::{Request, RequestBuilder};
use serde::de::DeserializeOwned;
use uuid::Uuid;
use yew::prelude::*;

use crate::utils::{self, FetchError, On401};

/// Which connection — the home portal, or one linked account on another
/// portal — a session was loaded through. Session ids are only unique within
/// one portal, so every piece of client state about a session is keyed by
/// [`SessionKey`] rather than the bare id: a linked portal advertising a home
/// session's id must never merge with it or receive its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(u32);

impl ConnId {
    pub const HOME: Self = Self(0);

    /// A non-home connection, for tests that need two portals side by side.
    pub(crate) const fn linked(n: u32) -> Self {
        Self(n)
    }
}

/// A session as seen through one connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionKey {
    pub conn: ConnId,
    pub id: Uuid,
}

impl SessionKey {
    pub fn home(id: Uuid) -> Self {
        Self {
            conn: ConnId::HOME,
            id,
        }
    }
}

/// `conn/id`, unique across every connection — usable as a Yew list key or a
/// DOM attribute where the bare id would collide between portals.
impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.conn.0, self.id)
    }
}

/// A portal origin (`scheme://host[:port]`, no trailing slash), the
/// connection it serves, and the means of authenticating to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalClient {
    origin: Rc<str>,
    conn: ConnId,
    linked: Option<crate::linked_credentials::LinkedCredentials>,
    available: bool,
    home_user: Option<Uuid>,
}

impl PortalClient {
    /// The portal that served this page, authenticated by its session cookie.
    pub fn home() -> Self {
        Self {
            origin: utils::get_base_url().into(),
            conn: ConnId::HOME,
            linked: None,
            available: true,
            home_user: None,
        }
    }

    /// A portal at a fixed origin, without reading `window.location`.
    #[cfg(test)]
    pub(crate) fn at(origin: &str, conn: ConnId) -> Self {
        Self {
            origin: origin.into(),
            conn,
            linked: None,
            available: true,
            home_user: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            origin: "".into(),
            conn: ConnId::HOME,
            linked: None,
            available: false,
            home_user: None,
        }
    }

    pub(crate) fn linked(
        origin: &str,
        conn: ConnId,
        credentials: crate::linked_credentials::LinkedCredentials,
    ) -> Result<Self, String> {
        let origin = shared::urls::canonical_origin(origin).map_err(|e| e.to_string())?;
        if conn == ConnId::HOME {
            return Err("A linked connection cannot use the home identity".into());
        }
        Ok(Self {
            origin: origin.into(),
            conn,
            linked: Some(credentials),
            available: true,
            home_user: None,
        })
    }

    pub fn home_for(user: Uuid) -> Self {
        Self {
            home_user: Some(user),
            ..Self::home()
        }
    }

    pub fn storage_key(&self, suffix: &str) -> Option<String> {
        self.account_id()
            .map(|user| format!("portal:{}:{}:{suffix}", self.origin, user))
    }

    pub fn is_linked(&self) -> bool {
        self.linked.is_some()
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn account_id(&self) -> Option<Uuid> {
        self.linked.as_ref().map(|c| c.user_id).or(self.home_user)
    }
    pub fn account_label(&self) -> Option<&str> {
        self.linked.as_ref().map(|c| c.email.as_str())
    }
    pub fn can_write(&self) -> bool {
        self.available
            && self
                .linked
                .as_ref()
                .is_none_or(|c| c.scope == shared::api::TokenScope::Write)
    }

    pub async fn media_blob(&self, path: &str) -> Result<web_sys::Blob, String> {
        if let Some(credentials) = &self.linked {
            credentials
                .refresh_if_needed(&self.origin)
                .await
                .map_err(|e| e.to_string())?;
        }
        let response = self.get(path).send().await.map_err(|e| e.to_string())?;
        if !response.ok() {
            return Err(format!("Media unavailable (HTTP {})", response.status()));
        }
        const MAX_BYTES: usize = 100 * 1024 * 1024;
        if response
            .headers()
            .get("content-length")
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|n| n > MAX_BYTES)
        {
            return Err("Media exceeds the 100 MB browser limit".into());
        }
        let mime = response
            .headers()
            .get("content-type")
            .unwrap_or_else(|| "application/octet-stream".into());
        let bytes = response.binary().await.map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Media exceeds the 100 MB browser limit".into());
        }
        let array = js_sys::Uint8Array::from(bytes.as_slice());
        let parts = js_sys::Array::of1(array.as_ref());
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(&mime);
        web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
            .map_err(|_| "Cannot decode media".into())
    }

    pub async fn revoke_link(&self) -> Result<(), FetchError> {
        let credentials = self.linked.as_ref().ok_or(FetchError::Status(403))?;
        credentials.refresh_if_needed(&self.origin).await?;
        let response = self
            .delete(&format!("/api/linked-grants/{}", credentials.grant_id))
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if response.status() != 204 {
            return Err(FetchError::Status(response.status()));
        }
        Ok(())
    }

    pub async fn socket_url(&self) -> Result<String, FetchError> {
        if !self.available {
            return Err(FetchError::Status(403));
        }
        let Some(credentials) = &self.linked else {
            return Ok(self.ws_url("/ws/client"));
        };
        let discovery = crate::federation::discover(&self.origin)
            .await
            .map_err(FetchError::Network)?;
        if discovery.instance_id != credentials.instance_id {
            return Err(FetchError::Decode(
                "The remote portal instance changed; authorize it again".into(),
            ));
        }
        credentials.refresh_if_needed(&self.origin).await?;
        let request = shared::api::WsTicketRequest {
            client_origin: credentials.client_origin.clone(),
        };
        let response = self
            .post("/api/ws-ticket")
            .json(&request)
            .map_err(|e| FetchError::Decode(e.to_string()))?
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if !response.ok() {
            return Err(FetchError::Status(response.status()));
        }
        let ticket: shared::api::WsTicketResponse = response
            .json()
            .await
            .map_err(|e| FetchError::Decode(e.to_string()))?;
        // Tickets are opaque URL-safe hex in protocol v1; reject URL syntax.
        if ticket.ticket.len() != 64 || !ticket.ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FetchError::Decode("Invalid socket ticket".into()));
        }
        Ok(format!(
            "{}?ticket={}",
            self.ws_url("/ws/client"),
            ticket.ticket
        ))
    }

    fn request(&self, path: &str, make: fn(&str) -> RequestBuilder) -> RequestBuilder {
        let valid = self.available && valid_path(path);
        let request = make(&self.url(path)).redirect(web_sys::RequestRedirect::Error);
        if let Some(credentials) = &self.linked {
            let request = request.credentials(web_sys::RequestCredentials::Omit);
            if valid {
                request.header(
                    "Authorization",
                    &format!("Bearer {}", credentials.secret.borrow().value),
                )
            } else {
                request
            }
        } else if let Some(user) = self.home_user {
            request.header("X-Portal-Account", &user.to_string())
        } else {
            request
        }
    }

    pub fn conn(&self) -> ConnId {
        self.conn
    }

    /// The key for session `id` on this connection.
    pub fn session_key(&self, id: Uuid) -> SessionKey {
        SessionKey {
            conn: self.conn,
            id,
        }
    }

    /// Absolute HTTP URL for an absolute `path` on this portal.
    pub fn url(&self, path: &str) -> String {
        if !self.available || !valid_path(path) {
            return "about:blank".into();
        }
        format!("{}{}", self.origin, path)
    }

    /// Absolute WebSocket URL for an absolute `path` on this portal.
    pub fn ws_url(&self, path: &str) -> String {
        format!("{}{}", shared::urls::http_to_ws(&self.origin), path)
    }

    /// Resolve a server-supplied reference (media `src`, file links): a
    /// root-relative path lands on this portal, anything else (absolute URL,
    /// `data:`) passes through untouched.
    pub fn resolve(&self, reference: &str) -> String {
        if reference.starts_with('/') && !reference.starts_with("//") {
            self.url(reference)
        } else {
            reference.to_string()
        }
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.request(path, Request::get)
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.request(path, Request::post)
    }

    pub fn patch(&self, path: &str) -> RequestBuilder {
        self.request(path, Request::patch)
    }

    pub fn delete(&self, path: &str) -> RequestBuilder {
        self.request(path, Request::delete)
    }

    /// GET `path` on this portal and decode the JSON response. The 401 policy
    /// matches [`utils::fetch_json`], except that only the home connection can
    /// log the page out: a linked portal rejecting its credential says nothing
    /// about the home session.
    pub async fn fetch_json<T: DeserializeOwned>(
        &self,
        path: &str,
        on_401: On401,
    ) -> Result<T, FetchError> {
        if let Some(credentials) = &self.linked {
            credentials.refresh_if_needed(&self.origin).await?;
        }
        let response = self
            .get(path)
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if response.status() == 401 && on_401 == On401::Logout && self.conn == ConnId::HOME {
            utils::logout();
            return Err(FetchError::Status(401));
        }
        if !response.ok() {
            return Err(FetchError::Status(response.status()));
        }
        response
            .json::<T>()
            .await
            .map_err(|e| FetchError::Decode(e.to_string()))
    }
}

/// The portal in scope for a function component. The app root always
/// provides one; a component rendered outside it (an isolated test render)
/// gets an unavailable client that cannot send requests.
#[hook]
pub fn use_portal() -> PortalClient {
    use_context::<PortalClient>().unwrap_or_else(PortalClient::unavailable)
}

/// The portal in scope for a struct component; see [`use_portal`].
pub fn portal_from_context<C: Component>(ctx: &Context<C>) -> PortalClient {
    ctx.link()
        .context::<PortalClient>(Callback::noop())
        .map(|(portal, _handle)| portal)
        .unwrap_or_else(PortalClient::unavailable)
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path
            .bytes()
            .any(|b| b == b'\\' || b == b'#' || b.is_ascii_control() || b.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portal(origin: &str) -> PortalClient {
        PortalClient::at(origin, ConnId::HOME)
    }

    #[test]
    fn invalid_request_targets_never_become_credential_destinations() {
        let p = portal("https://home.example");
        for path in [
            "https://evil.example/api/sessions",
            "//evil.example/api/sessions",
            "/\\evil.example",
            "/api/sessions#fragment",
            "/api/\nsessions",
        ] {
            assert_eq!(p.url(path), "about:blank");
        }
        assert_eq!(
            PortalClient::unavailable().url("/api/sessions"),
            "about:blank"
        );
    }

    #[test]
    fn saved_state_is_qualified_by_origin_and_account() {
        let user = Uuid::new_v4();
        let a = PortalClient {
            home_user: Some(user),
            ..portal("https://a.example")
        };
        let b = PortalClient {
            home_user: Some(user),
            ..portal("https://b.example")
        };
        let c = PortalClient {
            home_user: Some(Uuid::new_v4()),
            ..portal("https://a.example")
        };
        assert_ne!(a.storage_key("history"), b.storage_key("history"));
        assert_ne!(a.storage_key("history"), c.storage_key("history"));
        assert_eq!(portal("https://a.example").storage_key("history"), None);
    }

    #[test]
    fn session_keys_from_different_connections_never_collide() {
        let id = Uuid::new_v4();
        let home = portal("https://txcl.io").session_key(id);
        let linked = PortalClient::at("https://other.example", ConnId::linked(1)).session_key(id);

        assert_eq!(home, SessionKey::home(id));
        assert_ne!(home, linked);
        assert_ne!(home.to_string(), linked.to_string());
    }

    #[test]
    fn urls_join_origin_and_path() {
        let p = portal("https://txcl.io");
        assert_eq!(p.url("/api/sessions"), "https://txcl.io/api/sessions");
        assert_eq!(p.ws_url("/ws/client"), "wss://txcl.io/ws/client");
    }

    #[test]
    fn resolve_only_rewrites_root_relative_paths() {
        let p = portal("https://portal.cosmicfrontier.org");
        assert_eq!(
            p.resolve("/api/images/abc"),
            "https://portal.cosmicfrontier.org/api/images/abc"
        );
        assert_eq!(p.resolve("https://x.y/z.png"), "https://x.y/z.png");
        assert_eq!(p.resolve("//cdn.x/z.png"), "//cdn.x/z.png");
        assert_eq!(
            p.resolve("data:image/png;base64,AA"),
            "data:image/png;base64,AA"
        );
    }
}
