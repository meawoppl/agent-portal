//! Telling a session's members that it is no longer running, so their
//! dashboards refetch. Members get the bare `SessionExited` (no exit code,
//! no reason, no launcher error): only the launcher's owner learns why.

use diesel::prelude::*;
use shared::ServerToClient;
use tracing::warn;
use uuid::Uuid;

use super::SessionManager;

/// Send the bare `SessionExited` to `session_id`'s current members, except
/// `skip` (one already sent the detailed frames). Call only once the
/// session's disconnected status is committed, so a refetch reads it.
pub(super) fn notify_members_not_running(
    session_manager: &SessionManager,
    conn: &mut diesel::PgConnection,
    session_id: Uuid,
    skip: Option<Uuid>,
) {
    use crate::schema::session_members;
    let members = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .select(session_members::user_id)
        .load::<Uuid>(conn);
    for member in member_recipients(members, session_id, skip) {
        session_manager.broadcast_to_user(
            &member,
            ServerToClient::SessionExited {
                session_id,
                exit_code: None,
                reason: None,
            },
        );
    }
}

/// Who to tell from a membership lookup: everyone but `skip`, or no one if
/// the lookup failed. Never a wider broadcast.
fn member_recipients(
    members: diesel::QueryResult<Vec<Uuid>>,
    session_id: Uuid,
    skip: Option<Uuid>,
) -> Vec<Uuid> {
    match members {
        Ok(members) => members.into_iter().filter(|id| Some(*id) != skip).collect(),
        Err(e) => {
            warn!(
                "{} session={session_id}: {e}",
                crate::markers::MEMBER_STATUS_FANOUT_FAILED
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A membership lookup that fails tells no one, never a wider audience.
    #[test]
    fn member_lookup_failure_fails_closed() {
        let (owner, member) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            member_recipients(Ok(vec![owner, member]), Uuid::new_v4(), Some(owner)),
            [member]
        );
        assert_eq!(
            member_recipients(Ok(vec![owner, member]), Uuid::new_v4(), None),
            [owner, member]
        );
        assert!(member_recipients(
            Err(diesel::result::Error::BrokenTransactionManager),
            Uuid::new_v4(),
            Some(owner)
        )
        .is_empty());
    }
}
