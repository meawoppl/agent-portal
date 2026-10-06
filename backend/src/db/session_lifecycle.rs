//! Durable desired-session writes shared by launch handlers and reconciliation.
//!
//! Creation commits the session, fork metadata and owner membership together.
//! Tokens, websocket dispatch and queued input remain outside this transaction:
//! they involve live state and retain their existing compensation paths.
use crate::models::{NewSessionMember, NewSessionWithId};
use diesel::prelude::*;
use shared::{SessionRole, SessionStatus};
use uuid::Uuid;

pub(crate) struct DesiredSessionDraft {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub working_directory: String,
    pub session_name: String,
    pub hostname: String,
    pub launcher_id: Option<Uuid>,
    pub client_version: Option<String>,
    pub launcher_version: Option<String>,
    pub agent_type: shared::AgentType,
    pub claude_args: Vec<String>,
    pub plugin_overrides: Vec<shared::api::PluginOverride>,
    pub forked_from_session_id: Option<Uuid>,
    pub fork_point_turn_id: Option<String>,
    pub fork_create_worktree: bool,
}

pub(crate) fn create_desired_session(
    conn: &mut PgConnection,
    draft: DesiredSessionDraft,
) -> QueryResult<()> {
    conn.transaction(|conn| {
        use crate::schema::{session_members, sessions};
        use diesel::prelude::*;

        let new_session = NewSessionWithId {
            id: draft.session_id,
            user_id: draft.user_id,
            session_name: draft.session_name,
            session_key: draft.session_id.to_string(),
            working_directory: draft.working_directory,
            status: SessionStatus::Disconnected.as_str().to_string(),
            git_branch: None,
            client_version: draft.client_version,
            hostname: draft.hostname,
            launcher_id: draft.launcher_id,
            agent_type: draft.agent_type.as_str().to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::to_value(&draft.claude_args)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
            // Stamp the launcher's live version now — the registry entry that
            // holds it is gone by archive time (see
            // `NewSessionWithId::launcher_version`).
            launcher_version: draft.launcher_version,
        };

        diesel::insert_into(sessions::table)
            .values(&new_session)
            .execute(conn)?;

        if draft.forked_from_session_id.is_some() || draft.fork_point_turn_id.is_some() {
            diesel::update(sessions::table.find(draft.session_id))
                .set((
                    sessions::forked_from_session_id.eq(draft.forked_from_session_id),
                    sessions::fork_point_turn_id.eq(draft.fork_point_turn_id),
                    sessions::fork_launch_pending.eq(true),
                    sessions::fork_create_worktree.eq(draft.fork_create_worktree),
                ))
                .execute(conn)?;
        }

        diesel::insert_into(session_members::table)
            .values(NewSessionMember {
                session_id: draft.session_id,
                user_id: draft.user_id,
                role: SessionRole::Owner.as_str().to_string(),
            })
            .execute(conn)?;

        use crate::schema::session_plugin_overrides;
        diesel::insert_into(session_plugin_overrides::table)
            .values((
                session_plugin_overrides::session_id.eq(draft.session_id),
                session_plugin_overrides::overrides.eq(serde_json::to_value(
                    &draft.plugin_overrides,
                )
                .map_err(|error| diesel::result::Error::SerializationError(Box::new(error)))?),
            ))
            .execute(conn)?;

        Ok(())
    })
}

/// Persist a pause/stop before dispatching any process action.
pub(crate) fn pause(conn: &mut PgConnection, id: Uuid) -> QueryResult<usize> {
    use crate::schema::sessions;
    diesel::update(sessions::table.find(id))
        .set((
            sessions::paused.eq(true),
            sessions::status.eq(SessionStatus::Disconnected.as_str()),
            sessions::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)
}

/// Resume does not rewrite status, fork metadata or the launch lease.
pub(crate) fn resume(conn: &mut PgConnection, id: Uuid) -> QueryResult<usize> {
    use crate::schema::sessions;
    diesel::update(sessions::table.find(id))
        .set((
            sessions::paused.eq(false),
            sessions::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)
}

/// A failed resume dispatch restores only the desired pause bit.
pub(crate) fn cancel_resume(conn: &mut PgConnection, id: Uuid) -> QueryResult<usize> {
    use crate::schema::sessions;
    diesel::update(sessions::table.find(id))
        .set(sessions::paused.eq(true))
        .execute(conn)
}

/// Claim with one conditional UPDATE so concurrent reconcilers cannot both win.
/// Equality is deliberately still held; only a strictly expired lease is free.
pub(crate) fn claim_launch_lease(
    conn: &mut PgConnection,
    id: Uuid,
    now: chrono::NaiveDateTime,
    until: chrono::NaiveDateTime,
) -> QueryResult<usize> {
    use crate::schema::sessions;
    diesel::update(
        sessions::table.find(id).filter(
            sessions::launch_lease_until
                .is_null()
                .or(sessions::launch_lease_until.lt(now)),
        ),
    )
    .set(sessions::launch_lease_until.eq(Some(until)))
    .execute(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Session;
    use crate::schema::{session_members, sessions};
    use crate::test_support::{insert_session, insert_user, shared_pool};

    fn draft(user_id: Uuid) -> DesiredSessionDraft {
        DesiredSessionDraft {
            session_id: Uuid::new_v4(),
            user_id,
            working_directory: "/repo/project".into(),
            session_name: "desired session".into(),
            hostname: "launcher-host".into(),
            launcher_id: Some(Uuid::new_v4()),
            client_version: Some("client-test".into()),
            launcher_version: Some("launcher-test".into()),
            agent_type: shared::AgentType::Codex,
            claude_args: vec!["-c".into(), "model=test".into()],
            plugin_overrides: vec![],
            forked_from_session_id: None,
            fork_point_turn_id: None,
            fork_create_worktree: false,
        }
    }

    #[test]
    fn creation_preserves_metadata_and_commits_owner_with_fork_settings() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let owner = insert_user(conn, "desired-metadata");
            let source = insert_session(conn, owner.id, "source");
            for fork in [false, true] {
                let mut draft = draft(owner.id);
                let id = draft.session_id;
                let launcher = draft.launcher_id;
                if fork {
                    draft.forked_from_session_id = Some(source.id);
                    draft.fork_point_turn_id = Some("turn-42".into());
                    draft.fork_create_worktree = true;
                }
                create_desired_session(conn, draft)?;
                let row = sessions::table.find(id).first::<Session>(conn)?;
                assert_eq!(row.id, id);
                assert_eq!(row.session_key, id.to_string());
                assert_eq!(row.user_id, owner.id);
                assert_eq!(row.launcher_id, launcher);
                assert_eq!(row.session_name, "desired session");
                assert_eq!(row.working_directory, "/repo/project");
                assert_eq!(row.hostname, "launcher-host");
                assert_eq!(row.client_version.as_deref(), Some("client-test"));
                assert_eq!(row.launcher_version.as_deref(), Some("launcher-test"));
                assert_eq!(row.agent_type, "codex");
                assert_eq!(row.claude_args, serde_json::json!(["-c", "model=test"]));
                assert_eq!(row.status, SessionStatus::Disconnected.as_str());
                assert!(!row.paused);
                assert_eq!(row.forked_from_session_id, fork.then_some(source.id));
                assert_eq!(row.fork_point_turn_id.as_deref(), fork.then_some("turn-42"));
                assert_eq!(row.fork_launch_pending, fork);
                assert_eq!(row.fork_create_worktree, fork);
                assert!(row.launch_lease_until.is_none());
                let members = session_members::table
                    .filter(session_members::session_id.eq(id))
                    .select((session_members::user_id, session_members::role))
                    .load::<(Uuid, String)>(conn)?;
                assert_eq!(
                    members,
                    vec![(owner.id, SessionRole::Owner.as_str().into())]
                );
            }
            Ok(())
        });
    }

    #[test]
    fn membership_failure_rolls_back_session_and_fork_metadata() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let owner = insert_user(conn, "desired-rollback");
            let source = insert_session(conn, owner.id, "source");
            let mut draft = draft(owner.id);
            draft.forked_from_session_id = Some(source.id);
            draft.fork_point_turn_id = Some("turn-rollback".into());
            draft.fork_create_worktree = true;
            let id = draft.session_id;
            // This transaction-local trigger forces failure at the final write,
            // after both the session INSERT and fork UPDATE have succeeded.
            diesel::sql_query("CREATE FUNCTION pg_temp.reject_desired_owner() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected membership failure'; END $$").execute(conn)?;
            diesel::sql_query(format!("CREATE TRIGGER reject_desired_owner BEFORE INSERT ON session_members FOR EACH ROW WHEN (NEW.session_id = '{id}'::uuid) EXECUTE FUNCTION pg_temp.reject_desired_owner()" )).execute(conn)?;
            assert!(create_desired_session(conn, draft).is_err());
            assert_eq!(sessions::table.find(id).count().get_result::<i64>(conn)?, 0);
            assert_eq!(session_members::table.filter(session_members::session_id.eq(id)).count().get_result::<i64>(conn)?, 0);
            assert_eq!(sessions::table.find(source.id).count().get_result::<i64>(conn)?, 1);
            Ok(())
        });
    }

    #[test]
    fn pause_resume_and_lease_preserve_unrelated_session_state() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let owner = insert_user(conn, "desired-transitions");
            let mut draft = draft(owner.id);
            draft.fork_point_turn_id = Some("turn-kept".into());
            let id = draft.session_id;
            create_desired_session(conn, draft)?;
            let now =
                chrono::DateTime::from_timestamp_micros(chrono::Utc::now().timestamp_micros())
                    .unwrap()
                    .naive_utc();
            let until = now + chrono::Duration::seconds(30);
            assert_eq!(claim_launch_lease(conn, id, now, until)?, 1);
            assert_eq!(claim_launch_lease(conn, id, now, until)?, 0);
            assert_eq!(claim_launch_lease(conn, id, until, until)?, 0);
            pause(conn, id)?;
            assert!(sessions::table
                .find(id)
                .select(sessions::paused)
                .first::<bool>(conn)?);
            resume(conn, id)?;
            let resumed = sessions::table.find(id).first::<Session>(conn)?;
            assert!(!resumed.paused);
            assert_eq!(resumed.status, SessionStatus::Disconnected.as_str());
            cancel_resume(conn, id)?;
            let row = sessions::table.find(id).first::<Session>(conn)?;
            assert!(row.paused);
            assert_eq!(row.updated_at, resumed.updated_at);
            assert_eq!(row.launch_lease_until, Some(until));
            assert_eq!(row.fork_point_turn_id.as_deref(), Some("turn-kept"));
            assert!(row.fork_launch_pending);
            let later = until + chrono::Duration::seconds(1);
            assert_eq!(
                claim_launch_lease(conn, id, later, later + chrono::Duration::seconds(30))?,
                1
            );
            Ok(())
        });
    }
}
