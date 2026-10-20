//! Protected linked media is fetched with its connection bearer, never with
//! another account's browser cookie or a credential embedded in a URL.
use crate::portal_client::use_portal;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

pub(crate) fn protected_path(origin: &str, reference: &str) -> Option<String> {
    let path = reference.strip_prefix(origin).unwrap_or(reference);
    let endpoint = path.split('?').next().unwrap_or(path);
    let session_file = endpoint
        .strip_prefix("/api/sessions/")
        .and_then(|rest| rest.strip_suffix("/files/pull"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    // Attachment previews are protected file downloads too. Resolve them
    // through this portal's bearer rather than a cross-origin image cookie.
    (path.starts_with("/api/images/") || path.starts_with("/api/media/") || session_file)
        .then(|| path.to_string())
}

#[hook]
pub fn use_media_url(reference: &str) -> Result<Option<String>, String> {
    let portal = use_portal();
    let path = portal
        .is_linked()
        .then(|| protected_path(portal.origin(), reference))
        .flatten();
    let key = (portal.clone(), reference.to_string());
    let resolved = use_state(|| {
        None::<(
            (crate::portal_client::PortalClient, String),
            Result<String, String>,
        )>
    });
    {
        let resolved = resolved.clone();
        let path = path.clone();
        use_effect_with(key.clone(), move |key| {
            let (abort, registration) = futures_util::future::AbortHandle::new_pair();
            let cancelled = Rc::new(Cell::new(false));
            let allocated = Rc::new(RefCell::new(None::<String>));
            if let Some(path) = path {
                let (cancelled, allocated, key) =
                    (cancelled.clone(), allocated.clone(), key.clone());
                spawn_local(async move {
                    let _ = futures_util::future::Abortable::new(
                        async move {
                            let result = key.0.media_blob(&path).await;
                            if cancelled.get() {
                                return;
                            }
                            let url = result.and_then(|blob| {
                                web_sys::Url::create_object_url_with_blob(&blob)
                                    .map_err(|_| "Cannot create media URL".to_string())
                            });
                            if let Ok(url) = &url {
                                *allocated.borrow_mut() = Some(url.clone());
                            }
                            resolved.set(Some((key, url)));
                        },
                        registration,
                    )
                    .await;
                });
            }
            move || {
                cancelled.set(true);
                abort.abort();
                if let Some(url) = allocated.borrow_mut().take() {
                    let _ = web_sys::Url::revoke_object_url(&url);
                }
            }
        });
    }
    if path.is_none() {
        return Ok(Some(portal.resolve(reference)));
    }
    match &*resolved {
        Some((source, result)) if source == &key => result.clone().map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_previews_use_only_the_owning_portals_file_endpoint() {
        let origin = "https://portal.example";
        let path =
            "/api/sessions/00000000-0000-4000-8000-000000000001/files/pull?path=photo%20one.png";
        assert_eq!(protected_path(origin, path), Some(path.into()));
        assert_eq!(
            protected_path(origin, &format!("{origin}{path}")),
            Some(path.into())
        );
        for bad in [
            format!("https://evil.example{path}"),
            format!("//evil.example{path}"),
            "/api/sessions/not-an-id/files/pull?path=x.png".into(),
            "/api/sessions/00000000-0000-4000-8000-000000000001/files/push".into(),
        ] {
            assert_eq!(protected_path(origin, &bad), None);
        }
    }

    #[test]
    fn media_credentials_never_follow_foreign_references() {
        let origin = "https://portal.example";
        assert_eq!(
            protected_path(origin, "/api/images/a"),
            Some("/api/images/a".into())
        );
        assert_eq!(
            protected_path(origin, "https://portal.example/api/media/a"),
            Some("/api/media/a".into())
        );
        for reference in [
            "https://evil.example/api/images/a",
            "https://portal.example.evil/api/images/a",
            "//evil.example/api/images/a",
            "/api/admin/users",
            "data:image/png;base64,AA",
        ] {
            assert_eq!(protected_path(origin, reference), None);
        }
    }
}
