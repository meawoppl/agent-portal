//! Administrative policy changes apply to existing grants on their next use;
//! they do not silently rewrite a grant or mint a replacement credential.
use super::{load_policy, FederationPolicy};
use crate::{errors::AppError, schema::federation_policy, AppState};
use axum::{extract::State, http::HeaderMap, Json};
use diesel::prelude::*;
use std::sync::Arc;
use tower_cookies::Cookies;

const MAX_ORIGINS: usize = 64;

fn normalize(policy: FederationPolicy) -> Result<FederationPolicy, AppError> {
    match policy {
        FederationPolicy::Allowlist { origins, max_scope } => {
            if origins.is_empty() || origins.len() > MAX_ORIGINS {
                return Err(AppError::BadRequest(
                    "Provide between 1 and 64 allowed origins",
                ));
            }
            let mut origins = origins
                .into_iter()
                .map(|origin| {
                    shared::urls::canonical_origin(&origin).map_err(|_| {
                        AppError::BadRequest(
                            "Invalid allowed origin: use HTTPS scheme, host and optional port only",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            origins.sort();
            origins.dedup();
            Ok(FederationPolicy::Allowlist { origins, max_scope })
        }
        other => Ok(other),
    }
}

pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<FederationPolicy>, AppError> {
    super::super::admin::require_admin(&state, &headers, &cookies)?;
    Ok(Json(load_policy(&mut *state.conn()?)?))
}

pub async fn set_policy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(policy): Json<FederationPolicy>,
) -> Result<Json<FederationPolicy>, AppError> {
    let admin = super::super::admin::require_admin(&state, &headers, &cookies)?;
    let policy = normalize(policy)?;
    let value = serde_json::to_value(&policy).map_err(|e| AppError::Internal(e.to_string()))?;
    let updated =
        diesel::update(federation_policy::table.filter(federation_policy::singleton.eq(true)))
            .set(federation_policy::policy.eq(value))
            .execute(&mut *state.conn()?)?;
    if updated != 1 {
        return Err(AppError::ServiceUnavailable(
            "Federation policy is unavailable",
        ));
    }
    // Do not log origins: the allowlist can disclose private affiliations.
    tracing::info!(target: "auth_audit", event = "federation_policy_updated", admin_user_id = %admin.id);
    Ok(Json(policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::api::TokenScope;
    #[test]
    fn allowlist_is_strict_bounded_and_canonical() {
        let p = |origins| FederationPolicy::Allowlist {
            origins,
            max_scope: TokenScope::Read,
        };
        assert_eq!(
            normalize(p(vec![
                "https://HOME.example:443/".into(),
                "https://home.example".into()
            ]))
            .unwrap(),
            p(vec!["https://home.example".into()])
        );
        assert!(normalize(p(vec![])).is_err());
        assert!(normalize(p(vec!["https://home.example".into(); 65])).is_err());
        for origin in [
            "http://remote.example",
            "https://*.example",
            "https://x.example/path",
            "https://user@x.example",
            "ht\ntps://x.example",
        ] {
            assert!(normalize(p(vec![origin.into()])).is_err(), "{origin}");
        }
    }

    #[tokio::test]
    async fn only_native_admin_can_update_policy_and_invalid_update_is_atomic() {
        use crate::{
            handlers::proxy_tokens::{issue_proxy_token, TokenPersist},
            schema::users,
        };
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let _guard = super::super::TEST_POLICY_LOCK.lock().await;
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = Arc::new(crate::test_support::test_app_state(pool.clone()));
        let mut conn = pool.get().unwrap();
        let admin = crate::test_support::insert_user(&mut conn, "policy-admin");
        let user = crate::test_support::insert_user(&mut conn, "policy-user");
        diesel::update(users::table.find(admin.id))
            .set(users::is_admin.eq(true))
            .execute(&mut conn)
            .unwrap();
        let mut issue = |id| {
            issue_proxy_token(
                &mut conn,
                state.jwt_secret.as_bytes(),
                id,
                TokenPersist::Create {
                    name: "policy test",
                },
                Some(1),
            )
            .unwrap()
            .token
        };
        let admin_token = issue(admin.id);
        let user_token = issue(user.id);
        let app = crate::routes::build_router(state).unwrap();
        let request = |method: &str, token: &str, policy: &FederationPolicy| {
            Request::builder()
                .method(method)
                .uri("/api/admin/federation-policy")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(policy).unwrap()))
                .unwrap()
        };
        let enabled = FederationPolicy::Any {
            max_scope: TokenScope::Write,
        };
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", &user_token, &enabled))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", "ap_link_invalid", &enabled))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", &admin_token, &enabled))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let invalid = FederationPolicy::Allowlist {
            origins: vec!["https://*.example".into()],
            max_scope: TokenScope::Read,
        };
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", &admin_token, &invalid))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(load_policy(&mut conn).unwrap(), enabled);
        assert_eq!(
            app.clone()
                .oneshot(request("GET", &user_token, &enabled))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.oneshot(request("PUT", &admin_token, &FederationPolicy::Off))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        diesel::delete(users::table.filter(users::id.eq_any([user.id, admin.id])))
            .execute(&mut conn)
            .unwrap();
    }
}
