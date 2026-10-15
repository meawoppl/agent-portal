//! Short-lived, one-use socket credentials. The URL never carries the access
//! token. Persist hashes only; consume and revalidate in one transaction.
use super::{credentials::random_secret, LinkedAuth};
use crate::{
    errors::AppError,
    models::ProxyAuthToken,
    schema::{linked_ws_tickets as tickets, proxy_auth_tokens},
    AppState,
};
use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Extension, Json,
};
use diesel::prelude::*;
use shared::api::{TokenScope, WsTicketRequest, WsTicketResponse};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Queryable, Selectable)]
#[diesel(table_name = tickets)]
struct Ticket {
    #[allow(dead_code)]
    ticket_hash: String,
    token_id: Uuid,
    grant_id: Uuid,
    user_id: Uuid,
    client_origin: String,
    scope: String,
    expires_at: chrono::NaiveDateTime,
}

pub async fn issue(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<LinkedAuth>>,
    headers: HeaderMap,
    Json(request): Json<WsTicketRequest>,
) -> Result<Response, AppError> {
    let Extension(auth) = auth.ok_or(AppError::Forbidden)?;
    let declared =
        shared::urls::canonical_origin(&request.client_origin).map_err(|_| AppError::Forbidden)?;
    let header_origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| shared::urls::canonical_origin(v).ok())
        .ok_or(AppError::Forbidden)?;
    if declared != auth.client_origin || header_origin != declared {
        return Err(AppError::Forbidden);
    }
    auth.authorize(&state, TokenScope::Read)?;
    let ticket = random_secret();
    let now = chrono::Utc::now().naive_utc();
    let expires_at = now + chrono::Duration::seconds(30);
    let mut conn = state.conn()?;
    diesel::delete(tickets::table.filter(tickets::expires_at.lt(now))).execute(&mut conn)?;
    diesel::insert_into(tickets::table)
        .values((
            tickets::ticket_hash.eq(crate::jwt::hash_token(&ticket)),
            tickets::token_id.eq(auth.token_id),
            tickets::grant_id.eq(auth.grant_id),
            tickets::user_id.eq(auth.user_id),
            tickets::client_origin.eq(auth.client_origin),
            tickets::scope.eq(auth.scope.as_str()),
            tickets::expires_at.eq(expires_at),
        ))
        .execute(&mut conn)?;
    let mut response = Json(WsTicketResponse {
        ticket,
        expires_at: expires_at.and_utc().to_rfc3339(),
    })
    .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(response)
}

/// Called exclusively by /ws/client. Invalid tickets never fall back to a
/// cookie, legacy bearer or dev-mode identity. Do not log the ticket or URI.
pub fn consume(
    state: &AppState,
    secret: &str,
    headers: &HeaderMap,
) -> Result<LinkedAuth, AppError> {
    if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::Unauthorized);
    }
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| shared::urls::canonical_origin(v).ok())
        .ok_or(AppError::Forbidden)?;
    let mut conn = state.conn()?;
    conn.transaction::<_, AppError, _>(|conn| {
        // DELETE RETURNING is the atomic claim: two upgrades cannot both win.
        // Any failed validation rolls back, but cannot grant any authority.
        let ticket = diesel::delete(tickets::table.find(crate::jwt::hash_token(secret)))
            .get_result::<Ticket>(conn)
            .optional()?
            .ok_or(AppError::Unauthorized)?;
        if ticket.expires_at <= chrono::Utc::now().naive_utc() || ticket.client_origin != origin {
            return Err(AppError::Forbidden);
        }
        let token = proxy_auth_tokens::table
            .find(ticket.token_id)
            .first::<ProxyAuthToken>(conn)?;
        let auth = super::validate_row(conn, &token)?;
        if auth.user_id != ticket.user_id
            || auth.grant_id != ticket.grant_id
            || auth.client_origin != ticket.client_origin
            || Some(auth.scope) != TokenScope::parse(&ticket.scope)
        {
            return Err(AppError::Forbidden);
        }
        Ok(auth)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        handlers::federation::{credentials, Grant},
        schema::{federation_policy, linked_client_grants, users},
    };
    use axum::body::to_bytes;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message},
    };
    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, value.parse().unwrap());
        h
    }
    async fn mint(state: Arc<AppState>, auth: LinkedAuth) -> WsTicketResponse {
        let response = issue(
            State(state),
            Some(Extension(auth)),
            headers("https://home.example"),
            Json(WsTicketRequest {
                client_origin: "https://home.example".into(),
            }),
        )
        .await
        .unwrap();
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap()
    }
    #[tokio::test]
    async fn tickets_bind_origin_expiry_policy_and_allow_only_one_upgrade() {
        let _guard = super::super::TEST_POLICY_LOCK.lock().await;
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut app_state = crate::test_support::test_app_state(pool.clone());
        // Invalid tickets must not fall back to the permissive dev identity.
        app_state.dev_mode = true;
        let state = Arc::new(app_state);
        let mut conn = pool.get().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "ticket-owner");
        let policy = |conn: &mut PgConnection, value: shared::api::FederationPolicy| {
            diesel::update(federation_policy::table)
                .set(federation_policy::policy.eq(serde_json::to_value(value).unwrap()))
                .execute(conn)
                .unwrap();
        };
        policy(
            &mut conn,
            shared::api::FederationPolicy::Any {
                max_scope: TokenScope::Read,
            },
        );
        let grant = diesel::insert_into(linked_client_grants::table)
            .values((
                linked_client_grants::user_id.eq(user.id),
                linked_client_grants::client_origin.eq("https://home.example"),
                linked_client_grants::scope.eq("read"),
                linked_client_grants::expires_at
                    .eq(chrono::Utc::now().naive_utc() + chrono::Duration::days(1)),
            ))
            .get_result::<Grant>(&mut conn)
            .unwrap();
        let token = credentials::mint(&mut conn, &grant).unwrap();
        let auth = super::super::authenticate(
            &state,
            &mut conn,
            &token.access_token,
            &headers("https://home.example"),
        )
        .unwrap();
        assert!(issue(
            State(state.clone()),
            Some(Extension(auth.clone())),
            headers("https://evil.example"),
            Json(WsTicketRequest {
                client_origin: "https://home.example".into()
            })
        )
        .await
        .is_err());
        let ticket = mint(state.clone(), auth.clone()).await;
        assert!(consume(&state, &ticket.ticket, &headers("https://evil.example")).is_err());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let contender = || {
            let state = state.clone();
            let secret = ticket.ticket.clone();
            let barrier = barrier.clone();
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                consume(&state, &secret, &headers("https://home.example"))
            })
        };
        let (a, b) = tokio::join!(contender(), contender());
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(
            a.is_ok(),
            b.is_ok(),
            "one-use ticket has exactly one winner"
        );
        let expired = mint(state.clone(), auth.clone()).await;
        diesel::update(tickets::table.find(crate::jwt::hash_token(&expired.ticket)))
            .set(
                tickets::expires_at
                    .eq(chrono::Utc::now().naive_utc() - chrono::Duration::seconds(1)),
            )
            .execute(&mut conn)
            .unwrap();
        assert!(consume(&state, &expired.ticket, &headers("https://home.example")).is_err());
        let disabled = mint(state.clone(), auth.clone()).await;
        policy(&mut conn, shared::api::FederationPolicy::Off);
        assert!(consume(&state, &disabled.ticket, &headers("https://home.example")).is_err());
        policy(
            &mut conn,
            shared::api::FederationPolicy::Any {
                max_scope: TokenScope::Read,
            },
        );

        let app = crate::routes::build_router(state.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let request = |ticket: &str| {
            let mut r = format!("ws://{addr}/ws/client?ticket={ticket}")
                .into_client_request()
                .unwrap();
            r.headers_mut().insert(
                header::ORIGIN,
                HeaderValue::from_static("https://home.example"),
            );
            r
        };
        assert!(connect_async(request("invalid")).await.is_err());
        let live = mint(state.clone(), auth.clone()).await;
        let (mut socket, _) = connect_async(request(&live.ticket)).await.unwrap();
        assert!(connect_async(request(&live.ticket)).await.is_err());
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .unwrap();
        assert!(
            first.is_some_and(|r| r.is_ok()),
            "valid ticket receives user-channel data"
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&shared::ClientToServer::Interrupt)
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(Ok(frame)) = socket.next().await {
                if frame.is_close() {
                    break;
                }
            }
        })
        .await
        .expect("read-only mutation must close socket");

        let live = mint(state.clone(), auth.clone()).await;
        let (mut socket, _) = connect_async(request(&live.ticket)).await.unwrap();
        // Revoke while idle: the server must stop delivering and close promptly.
        diesel::update(linked_client_grants::table.find(grant.id))
            .set(linked_client_grants::revoked_at.eq(chrono::Utc::now().naive_utc()))
            .execute(&mut conn)
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(7), async {
            while let Some(Ok(frame)) = socket.next().await {
                if frame.is_close() {
                    break;
                }
            }
        })
        .await
        .expect("revoked idle socket must close");
        server.abort();
        policy(&mut conn, shared::api::FederationPolicy::Off);
        diesel::delete(users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }
}
