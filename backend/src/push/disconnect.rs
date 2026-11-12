//! Disconnect pushes concern interrupted work, not idle proxy connections.
use diesel::prelude::*;
use diesel::query_dsl::methods::FilterDsl;
use diesel::upsert::excluded;
use shared::{api::SessionActivityState, SessionStatus};
use uuid::Uuid;

use crate::{errors::AppError, handlers::session_activity::persisted_activity_states};

/// Claim one best-effort notification attempt per accepted input. Reconnecting
/// updates transport status but does not advance `input_seq`; only new input
/// rearms the alert. The database watermark survives dispatcher/backend restarts
/// and is shared by all disconnect producers and all push transports.
///
/// Call after preference, presence, mute and subscription checks so suppressed
/// events do not consume the claim. Claim before sending: a failed/partial
/// fan-out must not make a crash loop repeatedly notify successful devices.
pub(super) fn claim_disconnect(
    conn: &mut diesel::PgConnection,
    session_id: Uuid,
) -> Result<bool, AppError> {
    use crate::schema::{session_disconnect_notifications as claims, sessions};

    conn.transaction(|conn| {
        // Serialize claims with accepted input and status changes. A queued
        // event for a session which recovered, stopped or was deleted is stale.
        let Some((status, paused, input_seq)) = sessions::table
            .find(session_id)
            .select((sessions::status, sessions::paused, sessions::input_seq))
            .for_update()
            .first::<(String, bool, i64)>(conn)
            .optional()?
        else {
            return Ok(false);
        };
        if paused || status != SessionStatus::Disconnected.as_str() {
            return Ok(false);
        }

        // `active` means a proxy is connected, not that the agent has work.
        // Use persisted turn signals even though the proxy is now gone; the
        // connected-only UI activity helper would always report idle here.
        let activity = persisted_activity_states(conn, &[session_id])?;
        if activity.get(&session_id) != Some(&SessionActivityState::Busy) {
            return Ok(false);
        }

        let claimed = diesel::insert_into(claims::table)
            .values((
                claims::session_id.eq(session_id),
                claims::input_seq.eq(input_seq),
            ))
            .on_conflict(claims::session_id)
            .do_update()
            .set(claims::input_seq.eq(excluded(claims::input_seq)))
            .filter(claims::input_seq.lt(excluded(claims::input_seq)))
            .execute(conn)?;
        Ok(claimed == 1)
    })
}
