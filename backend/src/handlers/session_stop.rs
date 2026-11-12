//! Shared stop operation; callers retain their own authorization/policy checks.
use crate::{handlers::websocket::SessionManager, models::Session};
use diesel::{PgConnection, QueryResult};

/// Persist the desired state before touching the live process: heartbeat
/// reconciliation restarts unpaused sessions missing from the running set.
/// A failed write must therefore leave the process alone. Missing live peers
/// are fine: the durable do-not-relaunch state makes stopping idempotent.
pub(crate) fn persist_and_stop(
    conn: &mut PgConnection,
    manager: &SessionManager,
    session: &Session,
) -> QueryResult<()> {
    crate::db::session_lifecycle::pause(conn, session.id)?;
    manager.disconnect_session(session.id);
    manager.stop_session_on_launcher(
        session.id,
        session.launcher_id,
        Some(session.working_directory.clone()),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_session, insert_user, shared_pool};
    use diesel::prelude::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn failed_persistence_leaves_proxy_running_then_success_stops_it() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "stop-order");
        let session = insert_session(&mut conn, user.id, "stop-order");
        let manager = SessionManager::new();
        let (sender, mut receiver) = crate::handlers::websocket::conn_channel(8);
        manager.register_session(
            session.id.to_string().into(),
            sender,
            CancellationToken::new(),
        );

        // PostgreSQL rejects the UPDATE in an aborted transaction. No live
        // stop may escape when persistence fails.
        diesel::sql_query("BEGIN").execute(&mut conn).unwrap();
        assert!(diesel::sql_query("SELECT 1 / 0")
            .execute(&mut conn)
            .is_err());
        assert!(persist_and_stop(&mut conn, &manager, &session).is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), receiver.recv())
                .await
                .is_err()
        );
        diesel::sql_query("ROLLBACK").execute(&mut conn).unwrap();

        persist_and_stop(&mut conn, &manager, &session).unwrap();
        let paused: bool = crate::schema::sessions::table
            .find(session.id)
            .select(crate::schema::sessions::paused)
            .first(&mut conn)
            .unwrap();
        assert!(paused);
        assert!(matches!(
            receiver.recv().await,
            Some(shared::ServerToProxy::SessionTerminated { .. })
        ));
        // No launcher is connected; repeated stops still succeed.
        persist_and_stop(&mut conn, &manager, &session).unwrap();
    }
}
