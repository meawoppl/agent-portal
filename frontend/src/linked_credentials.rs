//! In-memory linked credentials. Never format or log an access token.
use crate::utils::FetchError;
use gloo_net::http::Request;
use shared::api::{LinkedClientRefreshResponse, TokenScope};
use std::{cell::RefCell, rc::Rc};
use uuid::Uuid;
use web_sys::{RequestCredentials, RequestRedirect};

#[derive(Clone)]
pub(crate) struct LinkedCredentials {
    pub user_id: Uuid,
    pub grant_id: Uuid,
    pub instance_id: Uuid,
    pub email: String,
    pub scope: TokenScope,
    pub client_origin: String,
    pub secret: Rc<RefCell<AccessToken>>,
    refresh_lock: Rc<futures_util::lock::Mutex<()>>,
}

pub(crate) struct AccessToken {
    pub value: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

impl LinkedCredentials {
    pub fn new(
        grant: shared::api::LinkedClientGrant,
        instance_id: Uuid,
        token: String,
        expires_at: &str,
    ) -> Result<Self, FetchError> {
        let expiry = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|e| FetchError::Decode(e.to_string()))?
            .with_timezone(&chrono::Utc);
        Ok(Self {
            user_id: grant.user_id,
            grant_id: grant.id,
            instance_id,
            email: grant.user_email,
            scope: grant.scope,
            client_origin: grant.client_origin,
            secret: Rc::new(RefCell::new(AccessToken {
                value: token,
                expires_at: expiry,
            })),
            refresh_lock: Rc::new(futures_util::lock::Mutex::new(())),
        })
    }

    /// Serialize rotation across simultaneous session/list/socket requests.
    /// Never retry a user mutation: refresh happens before dispatch, and a
    /// rejected mutation remains rejected for explicit user review.
    pub async fn refresh_if_needed(&self, origin: &str) -> Result<(), FetchError> {
        let _guard = self.refresh_lock.lock().await;
        if self.secret.borrow().expires_at > chrono::Utc::now() + chrono::Duration::seconds(120) {
            return Ok(());
        }
        let bearer = format!("Bearer {}", self.secret.borrow().value);
        let response = Request::post(&format!("{origin}/api/linked-grants/refresh"))
            .credentials(RequestCredentials::Omit)
            .redirect(RequestRedirect::Error)
            .header("Authorization", &bearer)
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if !response.ok() {
            return Err(FetchError::Status(response.status()));
        }
        let token: LinkedClientRefreshResponse = response
            .json()
            .await
            .map_err(|e| FetchError::Decode(e.to_string()))?;
        let expires_at = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
            .map_err(|e| FetchError::Decode(e.to_string()))?
            .with_timezone(&chrono::Utc);
        *self.secret.borrow_mut() = AccessToken {
            value: token.access_token,
            expires_at,
        };
        Ok(())
    }
}

impl PartialEq for LinkedCredentials {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.secret, &other.secret)
    }
}
impl Eq for LinkedCredentials {}
impl std::fmt::Debug for LinkedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkedCredentials")
            .field("user_id", &self.user_id)
            .field("grant_id", &self.grant_id)
            .finish_non_exhaustive()
    }
}
