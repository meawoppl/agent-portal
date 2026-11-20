//! Share a fresh generation check across browser frames already waiting for it.

use crate::{
    db::DbPool,
    schema::sessions,
    secrets::redaction::{self, Redactor},
};
use diesel::prelude::*;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot, OnceCell};
use uuid::Uuid;

const MAX_BATCH: usize = 128;
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default, Clone)]
pub(crate) struct BrowserRedaction {
    requests: Arc<OnceCell<mpsc::Sender<Request>>>,
}

struct Request {
    session_id: Option<Uuid>,
    reply: oneshot::Sender<Result<CheckedRules, CheckFailed>>,
}

pub(super) struct CheckedRules {
    pub(super) catalog: Arc<Redactor>,
    pub(super) owner: Option<Uuid>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum CheckFailed {
    PoolBusy,
    RulesUnavailable,
}

impl BrowserRedaction {
    pub(super) async fn check(
        &self,
        pool: &DbPool,
        session_id: Option<Uuid>,
    ) -> Result<CheckedRules, CheckFailed> {
        let requests = self
            .requests
            .get_or_init(|| async {
                let (tx, rx) = mpsc::channel(MAX_BATCH);
                tokio::spawn(run_batches(rx, pool.clone()));
                tx
            })
            .await;
        let (reply, result) = oneshot::channel();
        match requests.try_send(Request { session_id, reply }) {
            Ok(()) => result.await.map_err(|_| CheckFailed::RulesUnavailable)?,
            Err(mpsc::error::TrySendError::Full(_)) => {
                // A burst that fills the mailbox can still use the normal
                // bounded pool checkout instead of waiting behind many batches.
                read_rules(pool.clone(), session_id.into_iter().collect())
                    .await?
                    .for_session(session_id)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(CheckFailed::RulesUnavailable),
        }
    }
}

async fn run_batches(mut requests: mpsc::Receiver<Request>, pool: DbPool) {
    let mut batch = Vec::with_capacity(MAX_BATCH);
    while requests.recv_many(&mut batch, MAX_BATCH).await > 0 {
        batch.retain(|request| !request.reply.is_closed());
        if batch.is_empty() {
            continue;
        }

        // Seal the batch before reading the database. A request received
        // after this point has to wait for the next generation check.
        let session_ids = batch
            .iter()
            .filter_map(|request| request.session_id)
            .collect();
        let checked = read_rules(pool.clone(), session_ids).await;
        for request in batch.drain(..) {
            let result = match &checked {
                Ok(checked) => checked.for_session(request.session_id),
                Err(error) => Err(*error),
            };
            let _ = request.reply.send(result);
        }
    }
}

struct CatalogAndOwners {
    catalog: Arc<Redactor>,
    owners: HashMap<Uuid, Uuid>,
}

impl CatalogAndOwners {
    fn for_session(&self, session_id: Option<Uuid>) -> Result<CheckedRules, CheckFailed> {
        let owner = match session_id {
            Some(id) => Some(*self.owners.get(&id).ok_or(CheckFailed::RulesUnavailable)?),
            None => None,
        };
        Ok(CheckedRules {
            catalog: self.catalog.clone(),
            owner,
        })
    }
}

async fn read_rules(pool: DbPool, session_ids: Vec<Uuid>) -> Result<CatalogAndOwners, CheckFailed> {
    tokio::task::spawn_blocking(move || query_rules(&pool, session_ids))
        .await
        .unwrap_or(Err(CheckFailed::RulesUnavailable))
}

fn query_rules(pool: &DbPool, session_ids: Vec<Uuid>) -> Result<CatalogAndOwners, CheckFailed> {
    let mut conn = pool
        .get_timeout(CHECKOUT_TIMEOUT)
        .map_err(|_| CheckFailed::PoolBusy)?;
    let owners = if session_ids.is_empty() {
        HashMap::new()
    } else {
        sessions::table
            .filter(sessions::id.eq_any(session_ids))
            .select((sessions::id, sessions::user_id))
            .load::<(Uuid, Uuid)>(&mut conn)
            .map_err(|_| CheckFailed::RulesUnavailable)?
            .into_iter()
            .collect()
    };
    let catalog = redaction::snapshot(&mut conn).map_err(|_| CheckFailed::RulesUnavailable)?;
    Ok(CatalogAndOwners { catalog, owners })
}

#[cfg(test)]
pub(super) fn check_single_for_tests(
    pool: &DbPool,
    session_id: Option<Uuid>,
) -> Result<CheckedRules, CheckFailed> {
    query_rules(pool, session_id.into_iter().collect())?.for_session(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::secrets::tests::Database;
    use crate::schema::secrets_generation;
    use crate::test_support::{insert_session, insert_user};

    #[tokio::test]
    async fn batch_shares_one_catalog_and_later_requests_recheck_generation() {
        let Some(db) = Database::new() else { return };
        let (owner, session_id) = {
            let mut conn = db.pool.get().unwrap();
            let user = insert_user(&mut conn, "batch-owner");
            let session = insert_session(&mut conn, user.id, "batch-session");
            (user.id, session.id)
        };

        let (requests, inbox) = mpsc::channel(MAX_BATCH);
        let enqueue = |session_id| {
            let (reply, result) = oneshot::channel();
            assert!(requests.try_send(Request { session_id, reply }).is_ok());
            result
        };
        // Queue these before the worker starts so they share a definite batch.
        let scoped = enqueue(Some(session_id));
        let unscoped = enqueue(None);
        let missing = enqueue(Some(Uuid::new_v4()));
        let worker = tokio::spawn(run_batches(inbox, db.pool.clone()));
        let scoped = scoped.await.unwrap().unwrap();
        let unscoped = unscoped.await.unwrap().unwrap();
        assert_eq!(scoped.owner, Some(owner));
        assert!(unscoped.owner.is_none());
        assert!(Arc::ptr_eq(&scoped.catalog, &unscoped.catalog));
        assert!(matches!(
            missing.await.unwrap(),
            Err(CheckFailed::RulesUnavailable)
        ));

        // Commit a generation without publishing its matcher. A later batch
        // must fail closed, even while the old snapshot is still referenced.
        diesel::update(secrets_generation::table.find(1i16))
            .set(secrets_generation::generation.eq(1i64))
            .execute(&mut db.pool.get().unwrap())
            .unwrap();
        assert!(matches!(
            enqueue(Some(session_id)).await.unwrap(),
            Err(CheckFailed::RulesUnavailable)
        ));
        drop(requests);
        worker.await.unwrap();
    }
}
