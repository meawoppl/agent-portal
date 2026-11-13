use super::*;
use crate::test_support::{insert_user, shared_pool, test_app_state};
use tower::ServiceExt;

#[test]
fn retention_requires_explicit_unlimited() {
    assert_eq!(parse_retention("365").unwrap(), Some(365));
    assert_eq!(parse_retention("unlimited").unwrap(), None);
    for invalid in ["0", "-1", "", "forever", "36501"] {
        assert!(parse_retention(invalid).is_err());
    }
}
#[test]
fn cursor_requires_both_parts_and_valid_time() {
    assert!(cursor(&AuditQuery {
        before_id: Some(Uuid::new_v4()),
        ..Default::default()
    })
    .is_err());
    assert!(cursor(&AuditQuery {
        before_time: Some("invalid".into()),
        before_id: Some(Uuid::new_v4()),
        ..Default::default()
    })
    .is_err());
}
#[test]
fn stable_pagination_filters_and_transaction_rollback() {
    let Some(pool) = shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let actor = Uuid::new_v4();
    for action in [
        Action::Login,
        Action::Login,
        Action::Login,
        Action::ComputerRegistered,
    ] {
        record(&mut conn, Some(actor), Some(actor), action, "success", None).unwrap();
    }
    // Force tied timestamps to exercise the UUID half of the keyset cursor.
    diesel::update(events::table.filter(events::actor_user_id.eq(actor)))
        .set(events::occurred_at.eq(Utc::now()))
        .execute(&mut conn)
        .unwrap();
    let query = AuditQuery {
        actor: Some(actor),
        category: Some(AuditCategory::Identity),
        limit: Some(2),
        ..Default::default()
    };
    let first = page(&mut conn, query.clone(), Some(365)).unwrap();
    assert_eq!(first.events.len(), 2);
    assert!(first.has_more);
    let last = first.events.last().unwrap();
    let second = page(
        &mut conn,
        AuditQuery {
            before_time: Some(last.occurred_at.clone()),
            before_id: Some(last.id),
            ..query.clone()
        },
        Some(365),
    )
    .unwrap();
    assert_eq!(second.events.len(), 1);
    assert!(!second.has_more);
    assert!(!first.events.iter().any(|r| r.id == second.events[0].id));
    let result = conn.transaction::<(), AppError, _>(|conn| {
        record(
            conn,
            Some(actor),
            Some(actor),
            Action::Login,
            "success",
            None,
        )?;
        Err(AppError::Conflict("rollback"))
    });
    assert!(result.is_err());
    let all = page(
        &mut conn,
        AuditQuery {
            limit: Some(100),
            ..query
        },
        Some(365),
    )
    .unwrap();
    assert_eq!(all.events.len(), 3);
    diesel::delete(events::table.filter(events::actor_user_id.eq(actor)))
        .execute(&mut conn)
        .unwrap();
}
#[test]
fn legacy_secret_events_are_atomic_and_exclude_values() {
    let Some(pool) = shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = insert_user(&mut conn, "audit-secret");
    crate::secrets::store::audit_context(
        &mut conn,
        user.id,
        None,
        "deposit_proposed",
        "pending",
        Context {
            agent: true,
            operation: Some(Uuid::new_v4()),
            ..Default::default()
        },
    )
    .unwrap();
    let data = page(
        &mut conn,
        AuditQuery {
            actor: Some(user.id),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert_eq!(data.events.len(), 1);
    assert_eq!(data.events[0].actor_kind, "agent");
    assert!(data.events[0].operation_id.is_some());
    assert_eq!(data.events[0].details, AuditDetails::default());
    let result = conn.transaction::<(), AppError, _>(|conn| {
        crate::secrets::store::audit(conn, user.id, None, "deposit_decision", "approved")?;
        Err(AppError::Conflict("rollback"))
    });
    assert!(result.is_err());
    assert_eq!(
        page(
            &mut conn,
            AuditQuery {
                actor: Some(user.id),
                ..Default::default()
            },
            None
        )
        .unwrap()
        .events
        .len(),
        1
    );
}
#[test]
fn identity_events_survive_deletion_without_provider_subject_or_tokens() {
    let Some(pool) = shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = insert_user(&mut conn, "audit-identity");
    use crate::schema::user_identities as identities;
    diesel::insert_into(identities::table)
        .values((
            identities::user_id.eq(user.id),
            identities::provider.eq("google"),
            identities::subject.eq("sensitive-provider-subject"),
        ))
        .execute(&mut conn)
        .unwrap();
    diesel::delete(users::table.find(user.id))
        .execute(&mut conn)
        .unwrap();
    let rows = events::table
        .filter(events::owner_user_id.eq(user.id))
        .select((events::event_type, events::details))
        .load::<(String, serde_json::Value)>(&mut conn)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|(_, detail)| !detail.to_string().contains("sensitive-provider-subject")));
    assert!(rows.iter().any(|(kind, _)| kind == "identity.unlinked"));
}
#[tokio::test]
async fn admin_endpoint_rejects_anonymous_and_regular_users() {
    let Some(pool) = shared_pool() else {
        return;
    };
    let mut state = test_app_state(pool);
    state.dev_mode = false;
    let state = Arc::new(state);
    let user = insert_user(&mut state.conn().unwrap(), "audit-access");
    let token = crate::handlers::proxy_tokens::issue_proxy_token(
        &mut state.conn().unwrap(),
        state.jwt_secret.as_bytes(),
        user.id,
        crate::handlers::proxy_tokens::TokenPersist::Create { name: "audit test" },
        Some(1),
    )
    .unwrap()
    .token;
    let app = crate::routes::build_router(state.clone()).unwrap();
    for (auth, status) in [(false, 401), (true, 403)] {
        let mut req = axum::http::Request::builder().uri("/api/admin/audit");
        if auth {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let response = app
            .clone()
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    diesel::update(users::table.find(user.id))
        .set(users::is_admin.eq(true))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/admin/audit?limit=2")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 100_000)
        .await
        .unwrap();
    let page: AuditPage = serde_json::from_slice(&bytes).unwrap();
    assert!(page.events.len() <= 2);
}
#[tokio::test]
async fn polling_is_deduplicated_and_unlimited_retention_does_not_prune() {
    let Some(pool) = shared_pool() else {
        return;
    };
    let mut state = test_app_state(pool);
    state.audit_retention_days = None;
    let actor = Uuid::new_v4();
    let launcher = Uuid::new_v4();
    let flow = Uuid::new_v4();
    for _ in 0..3 {
        computer_login_result(&state, actor, launcher, flow, true);
    }
    let mut conn = state.conn().unwrap();
    assert_eq!(
        events::table
            .filter(events::operation_id.eq(flow))
            .count()
            .get_result::<i64>(&mut conn)
            .unwrap(),
        1
    );
    diesel::update(events::table.filter(events::operation_id.eq(flow)))
        .set(events::occurred_at.eq(Utc::now() - chrono::Duration::days(400)))
        .execute(&mut conn)
        .unwrap();
    sweep(Arc::new(state)).await;
    assert_eq!(
        events::table
            .filter(events::operation_id.eq(flow))
            .count()
            .get_result::<i64>(&mut conn)
            .unwrap(),
        1
    );
    prune(&mut conn, 365).unwrap();
    assert_eq!(
        events::table
            .filter(events::operation_id.eq(flow))
            .count()
            .get_result::<i64>(&mut conn)
            .unwrap(),
        0
    );
}
#[test]
fn token_rotation_and_revocation_do_not_record_token_hashes() {
    let Some(pool) = shared_pool() else {
        return;
    };
    let state = test_app_state(pool);
    let mut conn = state.conn().unwrap();
    let user = insert_user(&mut conn, "audit-token");
    let issued = crate::handlers::proxy_tokens::issue_proxy_token(
        &mut conn,
        state.jwt_secret.as_bytes(),
        user.id,
        crate::handlers::proxy_tokens::TokenPersist::Create {
            name: "do not log arbitrary labels",
        },
        Some(1),
    )
    .unwrap();
    use crate::schema::proxy_auth_tokens as tokens;
    diesel::update(tokens::table.find(issued.row_id))
        .set(tokens::revoked.eq(true))
        .execute(&mut conn)
        .unwrap();
    let rows = events::table
        .filter(events::target_id.eq(issued.row_id))
        .select((events::event_type, events::details))
        .load::<(String, serde_json::Value)>(&mut conn)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|(_, d)| d.as_object().is_some_and(|object| object.is_empty())));
}

#[test]
fn response_metadata_drops_unknown_fields() {
    let details: AuditDetails = serde_json::from_str(
        r#"{"provider":"google","token":"synthetic-sensitive-value","body":"do not return"}"#,
    )
    .unwrap();
    let encoded = serde_json::to_string(&details).unwrap();
    assert!(!encoded.contains("synthetic-sensitive-value"));
    assert!(!encoded.contains("do not return"));
}
