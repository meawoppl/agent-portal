//! Real-PG fault injection verifies the output publication boundary. Each
//! fixture has its own schema, so triggers cannot affect another test/session.
use super::*;
use crate::handlers::secrets::tests::{browser_session, request, Database};
use crate::schema::{messages, secrets_generation, session_attention};
use diesel::connection::SimpleConnection;
use std::sync::Arc;

const VALUE: &str = "synthetic-vault-password";
async fn fixture() -> Option<(Database, Arc<crate::AppState>, crate::models::Session)> {
    let db = Database::new()?;
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
        crate::secrets::crypto::EnvelopeCrypto {
            key: Arc::new(zeroize::Zeroizing::new(vec![42; 32])),
        },
    )));
    let user = crate::test_support::insert_user(&mut state.conn().unwrap(), "output_fence");
    let session =
        crate::test_support::insert_session(&mut state.conn().unwrap(), user.id, "output fence");
    let state = Arc::new(state);
    let token = browser_session(&state, user.id, "test human");
    let router = crate::handlers::secrets::router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, metadata) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"OUTPUT_TEST","value":VALUE}),
        Uuid::new_v4(),
    )
    .await;
    assert!(
        status.is_success(),
        "fixture vault creation failed: {metadata}"
    );
    Some((db, state, session))
}
fn publish(
    state: &crate::AppState,
    session: &crate::models::Session,
    tx: &ProxySender,
    notifications: &crate::push::NotificationSender,
    seq: u64,
    agent_type: AgentType,
    content: serde_json::Value,
) {
    handle_agent_output(
        AgentOutputContext {
            session_manager: &state.session_manager,
            session_key: &Some(session.session_key.clone().into()),
            db_session_id: Some(session.id),
            db_pool: &state.db_pool,
            notifications,
            tx,
            image_store: &state.image_store,
        },
        AgentOutputFrame {
            content,
            seq: Some(seq),
            agent_type,
        },
    );
}
fn count(state: &crate::AppState, session: Uuid) -> i64 {
    messages::table
        .filter(messages::session_id.eq(session))
        .count()
        .get_result(&mut state.conn().unwrap())
        .unwrap()
}
fn fault(state: &crate::AppState, table: &str) {
    assert!(["messages", "session_attention"].contains(&table));
    state.conn().unwrap().batch_execute(&format!("CREATE FUNCTION reject_secret_output() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic output write failure'; END $$; CREATE TRIGGER reject_secret_output BEFORE INSERT OR UPDATE ON {table} FOR EACH ROW EXECUTE FUNCTION reject_secret_output();")).unwrap();
}
fn clear_fault(state: &crate::AppState, table: &str) {
    state
        .conn()
        .unwrap()
        .batch_execute(&format!("DROP TRIGGER reject_secret_output ON {table}"))
        .unwrap();
}

#[tokio::test]
async fn failed_output_insert_has_no_publication_ack_or_sender_consumption_then_retries_once() {
    let Some((_db, state, session)) = fixture().await else {
        return;
    };
    let (mut tx, mut proxy) = super::super::conn_channel(16);
    let (web_tx, mut web) = super::super::conn_channel(16);
    state
        .session_manager
        .add_web_client(session.session_key.clone().into(), web_tx);
    let (notifications, mut pushes) = crate::push::channel();
    let sender = (session.user_id, "Human sender".to_string());
    state
        .session_manager
        .set_last_input_sender(session.id, sender.0, sender.1.clone());
    let content = serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":VALUE}]},"parent_tool_use_id":null});
    fault(&state, "messages");
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        7,
        AgentType::Claude,
        content.clone(),
    );
    assert_eq!(count(&state, session.id), 0);
    assert!(proxy.try_recv().is_err());
    assert!(web.try_recv().is_err());
    assert!(pushes.try_recv().is_err());
    assert_eq!(state.session_manager.last_ack_seq(session.id), 0);
    assert_eq!(
        state.session_manager.peek_last_input_sender(session.id),
        Some(sender)
    );
    assert!(
        proxy.cancellation_token().is_cancelled(),
        "failed output must close its cumulative-ACK connection"
    );
    (tx, proxy) = super::super::conn_channel(16);
    clear_fault(&state, "messages");
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        7,
        AgentType::Claude,
        content.clone(),
    );
    assert_eq!(count(&state, session.id), 1);
    assert_eq!(state.session_manager.last_ack_seq(session.id), 7);
    assert!(state
        .session_manager
        .peek_last_input_sender(session.id)
        .is_none());
    assert!(
        matches!(proxy.try_recv().unwrap(),ServerToProxy::OutputAck{session_id,ack_seq:7} if session_id==session.id)
    );
    let ServerToClient::AgentOutput {
        content: published,
        meta,
        ..
    } = web.try_recv().unwrap()
    else {
        panic!("expected committed output")
    };
    assert!(!published.to_string().contains(VALUE));
    assert!(published
        .to_string()
        .contains(crate::secrets::redaction::MASK));
    assert!(meta.is_some());
    let saved = messages::table
        .filter(messages::session_id.eq(session.id))
        .first::<crate::models::Message>(&mut state.conn().unwrap())
        .unwrap();
    assert!(!saved.content.contains(VALUE));
    assert_eq!(saved.user_id, session.user_id);
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        7,
        AgentType::Claude,
        content,
    );
    assert_eq!(count(&state, session.id), 1);
    assert!(web.try_recv().is_err());
    assert!(pushes.try_recv().is_err());
}

#[tokio::test]
async fn derived_attention_failure_rolls_back_output_without_ack_or_push() {
    let Some((_db, state, session)) = fixture().await else {
        return;
    };
    let (mut tx, mut proxy) = super::super::conn_channel(16);
    let (web_tx, mut web) = super::super::conn_channel(16);
    state
        .session_manager
        .add_web_client(session.session_key.clone().into(), web_tx);
    let (notifications, mut pushes) = crate::push::channel();
    let content = serde_json::json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1,"cached_input_tokens":0}});
    fault(&state, "session_attention");
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        3,
        AgentType::Codex,
        content.clone(),
    );
    assert_eq!(count(&state, session.id), 0);
    assert!(proxy.try_recv().is_err());
    assert!(web.try_recv().is_err());
    assert!(pushes.try_recv().is_err());
    assert_eq!(state.session_manager.last_ack_seq(session.id), 0);
    assert!(
        proxy.cancellation_token().is_cancelled(),
        "derived write failure must force replay on a fresh connection"
    );
    (tx, proxy) = super::super::conn_channel(16);
    clear_fault(&state, "session_attention");
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        3,
        AgentType::Codex,
        content.clone(),
    );
    assert_eq!(count(&state, session.id), 1);
    assert!(matches!(
        proxy.try_recv().unwrap(),
        ServerToProxy::OutputAck { ack_seq: 3, .. }
    ));
    assert!(matches!(
        web.try_recv().unwrap(),
        ServerToClient::AgentOutput { .. }
    ));
    assert!(
        matches!(pushes.try_recv().unwrap(),crate::push::NotificationEvent::TurnComplete{session_id,..} if session_id==session.id)
    );
    assert_eq!(
        session_attention::table
            .filter(session_attention::session_id.eq(session.id))
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        1
    );
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        3,
        AgentType::Codex,
        content,
    );
    assert_eq!(count(&state, session.id), 1);
    assert!(pushes.try_recv().is_err());
    assert!(web.try_recv().is_err());
}

#[tokio::test]
async fn generation_registration_waits_for_sanitized_content_transaction_commit() {
    let Some((_db, state, session)) = fixture().await else {
        return;
    };
    let mut writer = state.conn().unwrap();
    let mut reader = state.conn().unwrap();
    let before = secrets_generation::table
        .find(1i16)
        .select(secrets_generation::generation)
        .first::<i64>(&mut writer)
        .unwrap();
    reader
        .transaction::<_, diesel::result::Error, _>(|reader| {
            crate::secrets::redaction::lock_generation(reader).unwrap();
            let rules =
                crate::secrets::redaction::snapshot_for_user(reader, session.user_id).unwrap();
            let content = rules.redact_content(VALUE);
            diesel::insert_into(messages::table)
                .values(&crate::models::NewMessage {
                    session_id: session.id,
                    user_id: session.user_id,
                    role: "assistant".into(),
                    content,
                    agent_type: "claude".into(),
                    provenance_kind: None,
                    provenance_session_id: None,
                    provenance_agent_type: None,
                })
                .execute(reader)?;
            writer.batch_execute("SET lock_timeout = '50ms'").unwrap();
            let error = diesel::update(secrets_generation::table.find(1i16))
                .set(secrets_generation::generation.eq(secrets_generation::generation + 1))
                .execute(&mut writer)
                .unwrap_err();
            assert!(
                error.to_string().contains("lock timeout"),
                "registration must block on the content generation fence: {error}"
            );
            assert_eq!(
                secrets_generation::table
                    .find(1i16)
                    .select(secrets_generation::generation)
                    .first::<i64>(&mut writer)
                    .unwrap(),
                before
            );
            Ok(())
        })
        .unwrap();
    writer.batch_execute("SET lock_timeout = '0'").unwrap();
    diesel::update(secrets_generation::table.find(1i16))
        .set(secrets_generation::generation.eq(secrets_generation::generation + 1))
        .execute(&mut writer)
        .unwrap();
    assert_eq!(
        secrets_generation::table
            .find(1i16)
            .select(secrets_generation::generation)
            .first::<i64>(&mut writer)
            .unwrap(),
        before + 1
    );
    assert_eq!(count(&state, session.id), 1);
}

#[tokio::test]
async fn rolled_back_task_notification_does_not_double_count_subagent_usage_on_retry() {
    let Some((_db, state, session)) = fixture().await else {
        return;
    };
    let (mut tx, mut proxy) = super::super::conn_channel(16);
    let (notifications, _pushes) = crate::push::channel();
    let content = serde_json::json!({"type":"system","subtype":"task_notification","session_id":"native-session","task_id":"task-1","status":"completed","summary":"Synthetic task finished","output_file":null,"usage":{"duration_ms":10,"tool_uses":1,"total_tokens":11}});
    let Some(Ok(shared::ClaudeOutput::System(system))) =
        parse_claude_event(AgentType::Claude, &content)
    else {
        panic!("fixture must be a real task notification")
    };
    assert_eq!(
        system
            .as_task_notification()
            .unwrap()
            .usage
            .unwrap()
            .total_tokens,
        11
    );
    fault(&state, "messages");
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        5,
        AgentType::Claude,
        content.clone(),
    );
    assert_eq!(state.session_manager.subagent_tokens(session.id), 0);
    assert_eq!(count(&state, session.id), 0);
    assert!(proxy.try_recv().is_err());
    assert!(
        proxy.cancellation_token().is_cancelled(),
        "failed output must close its cumulative-ACK connection"
    );
    (tx, proxy) = super::super::conn_channel(16);
    clear_fault(&state, "messages");
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        5,
        AgentType::Claude,
        content.clone(),
    );
    assert_eq!(state.session_manager.subagent_tokens(session.id), 11);
    assert_eq!(count(&state, session.id), 1);
    assert!(matches!(
        proxy.try_recv().unwrap(),
        ServerToProxy::OutputAck { ack_seq: 5, .. }
    ));
    publish(
        &state,
        &session,
        &tx,
        &notifications,
        5,
        AgentType::Claude,
        content,
    );
    assert_eq!(state.session_manager.subagent_tokens(session.id), 11);
    assert_eq!(count(&state, session.id), 1);
}
