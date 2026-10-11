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
    #[cfg(test)]
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
}

impl PortalClient {
    /// The portal that served this page, authenticated by its session cookie.
    pub fn home() -> Self {
        Self {
            origin: utils::get_base_url().into(),
            conn: ConnId::HOME,
        }
    }

    /// A portal at a fixed origin, without reading `window.location`.
    #[cfg(test)]
    pub(crate) fn at(origin: &str, conn: ConnId) -> Self {
        Self {
            origin: origin.into(),
            conn,
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
        Request::get(&self.url(path))
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        Request::post(&self.url(path))
    }

    pub fn patch(&self, path: &str) -> RequestBuilder {
        Request::patch(&self.url(path))
    }

    pub fn delete(&self, path: &str) -> RequestBuilder {
        Request::delete(&self.url(path))
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
/// gets the home portal.
#[hook]
pub fn use_portal() -> PortalClient {
    use_context::<PortalClient>().unwrap_or_else(PortalClient::home)
}

/// The portal in scope for a struct component; see [`use_portal`].
pub fn portal_from_context<C: Component>(ctx: &Context<C>) -> PortalClient {
    ctx.link()
        .context::<PortalClient>(Callback::noop())
        .map(|(portal, _handle)| portal)
        .unwrap_or_else(PortalClient::home)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portal(origin: &str) -> PortalClient {
        PortalClient::at(origin, ConnId::HOME)
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
