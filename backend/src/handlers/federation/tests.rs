use super::*;

#[test]
fn authority_minting_and_unclassified_routes_are_denied() {
    for route in [
        "/api/proxy-tokens",
        "/api/proxy-tokens/{id}/renew",
        "/api/auth/refresh",
        "/api/portal-connections",
        "/api/sessions/{id}/forwards/open",
        "/api/admin/users",
        "/ws/client",
        "/ws/launcher",
        "/ws/session",
        "/api/new-api",
    ] {
        for method in [Method::GET, Method::POST, Method::DELETE, Method::PATCH] {
            assert_eq!(route_scope(&method, route), None, "{method} {route}");
        }
    }
    assert_eq!(
        route_scope(&Method::GET, "/api/sessions"),
        Some(TokenScope::Read)
    );
    assert_eq!(
        route_scope(&Method::POST, "/api/sessions/{id}/stop"),
        Some(TokenScope::Write)
    );
    assert_eq!(
        message_scope(&shared::ClientToServer::Interrupt),
        TokenScope::Write
    );
}

#[test]
fn policy_caps_and_exact_origins_fail_closed() {
    let policy = FederationPolicy::Allowlist {
        origins: vec!["https://home.example".into()],
        max_scope: TokenScope::Read,
    };
    assert!(policy_allows(
        &policy,
        "https://home.example",
        TokenScope::Read
    ));
    assert!(!policy_allows(
        &policy,
        "https://home.example",
        TokenScope::Write
    ));
    assert!(!policy_allows(
        &policy,
        "https://home.example.evil",
        TokenScope::Read
    ));
    assert!(!policy_allows(
        &FederationPolicy::Off,
        "https://home.example",
        TokenScope::Read
    ));
}

#[test]
fn linked_credentials_keep_their_ceiling_and_cannot_become_legacy_tokens() {
    let _guard = super::TEST_POLICY_LOCK.blocking_lock();
    let Some(pool) = crate::test_support::shared_pool() else {
        return;
    };
    let state = crate::test_support::test_app_state(pool.clone());
    let mut conn = pool.get().unwrap();
    conn.test_transaction::<_, AppError, _>(|conn| {
        let user = crate::test_support::insert_user(conn, "linked-auth");
        let policy = FederationPolicy::Any {
            max_scope: TokenScope::Write,
        };
        diesel::update(federation_policy::table)
            .set(federation_policy::policy.eq(serde_json::to_value(policy).unwrap()))
            .execute(conn)?;
        let grant: Grant = diesel::insert_into(linked_client_grants::table)
            .values((
                linked_client_grants::user_id.eq(user.id),
                linked_client_grants::client_origin.eq("https://home.example"),
                linked_client_grants::scope.eq("read"),
                linked_client_grants::expires_at
                    .eq(chrono::Utc::now().naive_utc() + chrono::Duration::hours(1)),
            ))
            .get_result(conn)?;
        let bearer = format!("{LINKED_TOKEN_PREFIX}{}", Uuid::new_v4());
        let token: ProxyAuthToken = diesel::insert_into(proxy_auth_tokens::table)
            .values((
                proxy_auth_tokens::user_id.eq(user.id),
                proxy_auth_tokens::name.eq("linked test"),
                proxy_auth_tokens::token_hash.eq(crate::jwt::hash_token(&bearer)),
                proxy_auth_tokens::scope.eq("read"),
                proxy_auth_tokens::linked_grant_id.eq(grant.id),
                proxy_auth_tokens::expires_at.eq(grant.expires_at),
            ))
            .get_result(conn)?;
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://home.example".parse().unwrap());
        assert_eq!(
            authenticate(&state, conn, &bearer, &headers)?.scope,
            TokenScope::Read
        );
        assert!(crate::handlers::proxy_tokens::verify_and_get_user(&state, conn, &bearer).is_err());
        assert!(crate::handlers::proxy_tokens::issue_proxy_token(
            conn,
            state.jwt_secret.as_bytes(),
            user.id,
            crate::handlers::proxy_tokens::TokenPersist::Renew { token_id: token.id },
            Some(30)
        )
        .is_err());
        headers.insert("origin", "https://wrong.example".parse().unwrap());
        assert!(authenticate(&state, conn, &bearer, &headers).is_err());
        headers.remove("origin");
        assert!(authenticate(&state, conn, &bearer, &headers).is_err());
        // DB scope escalation beyond the grant is rejected even with valid token bytes.
        diesel::update(proxy_auth_tokens::table.find(token.id))
            .set(proxy_auth_tokens::scope.eq("write"))
            .execute(conn)?;
        let elevated = proxy_auth_tokens::table.find(token.id).first(conn)?;
        assert!(validate_row(conn, &elevated).is_err());
        diesel::update(federation_policy::table)
            .set(federation_policy::policy.eq(serde_json::to_value(FederationPolicy::Off).unwrap()))
            .execute(conn)?;
        assert!(validate_row(conn, &token).is_err());
        diesel::update(federation_policy::table)
            .set(
                federation_policy::policy.eq(serde_json::to_value(FederationPolicy::Any {
                    max_scope: TokenScope::Write,
                })
                .unwrap()),
            )
            .execute(conn)?;
        diesel::update(linked_client_grants::table.find(grant.id))
            .set(linked_client_grants::revoked_at.eq(chrono::Utc::now().naive_utc()))
            .execute(conn)?;
        assert!(validate_row(conn, &token).is_err());
        Ok(())
    });
}

/// Exercise the real router, not just the classifier: MatchedPath must be
/// available at the gate and a linked bearer must not fall back to dev login.
#[tokio::test]
async fn router_applies_scope_before_handlers_and_live_revocation() {
    let _guard = super::TEST_POLICY_LOCK.lock().await;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let Some(pool) = crate::test_support::shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = crate::test_support::insert_user(&mut conn, "linked-router");
    let other = crate::test_support::insert_user(&mut conn, "other-owner");
    let session = crate::test_support::insert_session(&mut conn, other.id, "foreign-session");
    let policy = serde_json::to_value(FederationPolicy::Any {
        max_scope: TokenScope::Write,
    })
    .unwrap();
    diesel::update(federation_policy::table)
        .set(federation_policy::policy.eq(policy))
        .execute(&mut conn)
        .unwrap();
    let grant: Grant = diesel::insert_into(linked_client_grants::table)
        .values((
            linked_client_grants::user_id.eq(user.id),
            linked_client_grants::client_origin.eq("https://home.example"),
            linked_client_grants::scope.eq("write"),
            linked_client_grants::expires_at
                .eq(chrono::Utc::now().naive_utc() + chrono::Duration::hours(1)),
        ))
        .get_result(&mut conn)
        .unwrap();
    let bearer = format!("{LINKED_TOKEN_PREFIX}{}", Uuid::new_v4());
    let token: ProxyAuthToken = diesel::insert_into(proxy_auth_tokens::table)
        .values((
            proxy_auth_tokens::user_id.eq(user.id),
            proxy_auth_tokens::name.eq("router test"),
            proxy_auth_tokens::token_hash.eq(crate::jwt::hash_token(&bearer)),
            proxy_auth_tokens::scope.eq("read"),
            proxy_auth_tokens::linked_grant_id.eq(grant.id),
            proxy_auth_tokens::expires_at.eq(grant.expires_at),
        ))
        .get_result(&mut conn)
        .unwrap();
    let mut state = crate::test_support::test_app_state(pool.clone());
    state.dev_mode = true;
    state.forward_domain = Some("forward.example".into());
    let state = Arc::new(state);
    let app = crate::routes::build_router(state).unwrap();
    let request = |method: &str, path: &str, host: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {bearer}"))
            .header("origin", "https://home.example")
            .header("host", host)
            .body(Body::empty())
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(request("GET", "/api/sessions", "portal.example"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    for (method, path) in [
        ("GET", "/api/portal-connections"),
        ("POST", "/api/proxy-tokens"),
        ("GET", "/ws/client"),
        ("GET", "/not-an-api"),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, path, "portal.example"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let stop = format!("/api/sessions/{}/stop", session.id);
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &stop, "portal.example"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // The router ceiling never substitutes for the handler's membership gate.
    diesel::update(proxy_auth_tokens::table.find(token.id))
        .set(proxy_auth_tokens::scope.eq("write"))
        .execute(&mut conn)
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &stop, "portal.example"))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    use crate::schema::session_members;
    diesel::insert_into(session_members::table)
        .values((
            session_members::session_id.eq(session.id),
            session_members::user_id.eq(user.id),
            session_members::role.eq("viewer"),
        ))
        .execute(&mut conn)
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &stop, "portal.example"))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/api/sessions", "demo.forward.example"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    diesel::update(linked_client_grants::table.find(grant.id))
        .set(linked_client_grants::revoked_at.eq(chrono::Utc::now().naive_utc()))
        .execute(&mut conn)
        .unwrap();
    assert_eq!(
        app.oneshot(request("GET", "/api/sessions", "portal.example"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    diesel::delete(users::table.filter(users::id.eq_any([user.id, other.id])))
        .execute(&mut conn)
        .unwrap();
    diesel::update(federation_policy::table)
        .set(federation_policy::policy.eq(serde_json::to_value(FederationPolicy::Off).unwrap()))
        .execute(&mut conn)
        .unwrap();
}
