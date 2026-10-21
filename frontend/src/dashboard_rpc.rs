//! Bounded correlation for the home account's shared user socket. A disconnect
//! fails pending calls, rather than replaying a launch/install/restart twice.
use futures_channel::{mpsc, oneshot};
use futures_util::{
    future::{select, Either},
    FutureExt,
};
use shared::{
    api::{DashboardError, DashboardRequest, DashboardResponse},
    ClientToServer,
};
use std::{cell::RefCell, collections::HashMap};
use uuid::Uuid;

pub type Reply = Result<DashboardResponse, DashboardError>;
#[derive(Default)]
struct Broker {
    account: Option<Uuid>,
    generation: u64,
    sender: Option<mpsc::UnboundedSender<ClientToServer>>,
    pending: HashMap<Uuid, oneshot::Sender<Reply>>,
    ready: Vec<oneshot::Sender<()>>,
}
thread_local! { static BROKER: RefCell<Broker> = RefCell::default(); }

/// Clear queued work before a different authenticated account is rendered.
pub fn bind_account(account: Option<Uuid>) {
    BROKER.with_borrow_mut(|broker| {
        if broker.account == account {
            return;
        }
        broker.account = account;
        broker.generation = broker.generation.wrapping_add(1);
        broker.sender = None;
        broker.ready.clear();
        for (_, waiter) in broker.pending.drain() {
            let _ = waiter.send(Err(error(401, "Account changed; request cancelled")));
        }
    });
}

pub fn connect(sender: mpsc::UnboundedSender<ClientToServer>) -> u64 {
    BROKER.with_borrow_mut(|broker| {
        broker.generation = broker.generation.wrapping_add(1);
        broker.sender = Some(sender);
        for waiter in broker.ready.drain(..) {
            let _ = waiter.send(());
        }
        broker.generation
    })
}
pub fn disconnect(generation: u64) {
    BROKER.with_borrow_mut(|broker| {
        if broker.generation != generation {
            return;
        }
        broker.sender = None;
        for (_, waiter) in broker.pending.drain() {
            let _ = waiter.send(Err(error(
                503,
                "Connection lost. The operation may have completed; check before retrying.",
            )));
        }
    });
}
pub fn resolve(id: Uuid, result: Reply) {
    BROKER.with_borrow_mut(|broker| {
        if let Some(waiter) = broker.pending.remove(&id) {
            let _ = waiter.send(result);
        }
    });
}
fn error(status: u16, message: &str) -> DashboardError {
    DashboardError {
        status,
        message: message.into(),
    }
}

struct Pending(Uuid);
impl Drop for Pending {
    fn drop(&mut self) {
        BROKER.with_borrow_mut(|b| {
            b.pending.remove(&self.0);
        });
    }
}

pub async fn request(request: DashboardRequest) -> Reply {
    let ready = BROKER.with_borrow_mut(|broker| {
        if broker.sender.is_some() {
            return None;
        }
        broker.ready.retain(|waiter| !waiter.is_canceled());
        let (tx, rx) = oneshot::channel();
        if broker.ready.len() < 32 {
            broker.ready.push(tx);
        }
        Some(rx)
    });
    if let Some(ready) = ready {
        if !matches!(
            select(ready, gloo::timers::future::TimeoutFuture::new(10_000)).await,
            Either::Left((Ok(()), _))
        ) {
            return Err(error(
                503,
                "Dashboard connection unavailable. Please reconnect and try again.",
            ));
        }
    }
    let id = Uuid::new_v4();
    let (tx, rx) = oneshot::channel();
    BROKER.with_borrow_mut(|broker| {
        if broker.pending.len() >= 32 {
            return Err(error(429, "Too many pending dashboard requests"));
        }
        let sender = broker
            .sender
            .as_ref()
            .ok_or_else(|| error(503, "Dashboard disconnected"))?;
        sender
            .unbounded_send(ClientToServer::Dashboard {
                request_id: id,
                request,
            })
            .map_err(|_| error(503, "Dashboard disconnected"))?;
        broker.pending.insert(id, tx);
        Ok(())
    })?;
    let _pending = Pending(id);
    match select(
        rx,
        gloo::timers::future::TimeoutFuture::new(195_000).boxed_local(),
    )
    .await
    {
        Either::Left((Ok(result), _)) => result,
        _ => Err(error(
            504,
            "No reply received. Check whether the operation completed before retrying.",
        )),
    }
}

pub async fn launchers() -> Result<Vec<shared::LauncherInfo>, crate::utils::FetchError> {
    match request(DashboardRequest::ListLaunchers)
        .await
        .map_err(|e| crate::utils::FetchError::Status(e.status))?
    {
        DashboardResponse::Launchers(rows) => Ok(rows),
        _ => Err(crate::utils::FetchError::Decode(
            "Unexpected dashboard reply".into(),
        )),
    }
}
pub async fn probe(
    launcher_id: Uuid,
) -> Result<shared::api::ProbeAgentsResponse, crate::utils::FetchError> {
    match request(DashboardRequest::ProbeAgents { launcher_id })
        .await
        .map_err(|e| crate::utils::FetchError::Status(e.status))?
    {
        DashboardResponse::Agents(rows) => Ok(rows),
        _ => Err(crate::utils::FetchError::Decode(
            "Unexpected dashboard reply".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        BROKER.with_borrow_mut(|b| *b = Broker::default());
    }

    #[test]
    fn responses_resolve_by_id_not_arrival_order() {
        reset();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let (tx_a, mut rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        BROKER.with_borrow_mut(|b| {
            b.pending.insert(first, tx_a);
            b.pending.insert(second, tx_b);
        });
        resolve(second, Ok(DashboardResponse::Complete));
        assert!(matches!(
            rx_b.try_recv(),
            Ok(Some(Ok(DashboardResponse::Complete)))
        ));
        assert!(rx_a.try_recv().unwrap().is_none());
        resolve(first, Err(error(403, "Forbidden")));
        assert_eq!(rx_a.try_recv().unwrap().unwrap().unwrap_err().status, 403);
        BROKER.with_borrow(|b| assert!(b.pending.is_empty()));
    }

    #[test]
    fn disconnect_fails_pending_without_replaying_and_old_cleanup_cannot_close_new_socket() {
        reset();
        let (sender, mut requests) = mpsc::unbounded();
        let old = connect(sender);
        let (tx, mut rx) = oneshot::channel();
        BROKER.with_borrow_mut(|b| {
            b.pending.insert(Uuid::new_v4(), tx);
        });
        disconnect(old);
        assert_eq!(rx.try_recv().unwrap().unwrap().unwrap_err().status, 503);
        assert!(requests.try_recv().is_err());
        let (sender, _requests) = mpsc::unbounded();
        let new = connect(sender);
        disconnect(old);
        BROKER.with_borrow(|b| {
            assert!(b.sender.is_some());
            assert_eq!(b.generation, new);
            assert!(b.pending.is_empty());
        });
        disconnect(new);
    }

    #[test]
    fn dropped_waiter_frees_correlation_and_late_reply_is_ignored() {
        reset();
        let id = Uuid::new_v4();
        let (tx, _rx) = oneshot::channel();
        BROKER.with_borrow_mut(|b| {
            b.pending.insert(id, tx);
        });
        drop(Pending(id));
        resolve(id, Ok(DashboardResponse::Complete));
        BROKER.with_borrow(|b| assert!(b.pending.is_empty()));
    }
}
