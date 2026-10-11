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
use yew::prelude::*;

use crate::utils::{self, FetchError, On401};

/// A portal origin (`scheme://host[:port]`, no trailing slash) and the means
/// of authenticating to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalClient {
    origin: Rc<str>,
}

impl PortalClient {
    /// The portal that served this page, authenticated by its session cookie.
    pub fn home() -> Self {
        Self {
            origin: utils::get_base_url().into(),
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

    /// GET `path` on this portal and decode the JSON response; the 401
    /// policy matches [`utils::fetch_json`].
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
        if response.status() == 401 && on_401 == On401::Logout {
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
        PortalClient {
            origin: origin.into(),
        }
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
