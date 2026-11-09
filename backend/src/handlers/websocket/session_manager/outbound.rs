//! Outbound budgets include the frame currently being written, not just the
//! queue. A receiver releases its previous permits only on the next receive.
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const CONNECTION_BYTES: usize = 64 * 1024 * 1024;
const PROCESS_BYTES: usize = 256 * 1024 * 1024;

pub trait OutboundMessage {
    fn outbound_bytes(&self) -> Result<usize, ()>;
}

struct Counter(usize);
impl std::io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
macro_rules! json_weight {
    ($($ty:ty),*) => { $(impl OutboundMessage for $ty {
        fn outbound_bytes(&self) -> Result<usize, ()> {
            let mut counter = Counter(0);
            serde_json::to_writer(&mut counter, self).map_err(|_| ())?;
            Ok(counter.0.max(1))
        }
    })* };
}
json_weight!(
    shared::ServerToProxy,
    shared::ServerToClient,
    shared::ServerToLauncher
);
impl OutboundMessage for shared::TunnelFrame {
    fn outbound_bytes(&self) -> Result<usize, ()> {
        use ws_bridge::WsCodec;
        match self.encode().map_err(|_| ())? {
            ws_bridge::WsMessage::Text(bytes) => Ok(bytes.len().max(1)),
            ws_bridge::WsMessage::Binary(bytes) => Ok(bytes.len().max(1)),
        }
    }
}

type Permits = (OwnedSemaphorePermit, OwnedSemaphorePermit);
struct Queued<T> {
    value: T,
    permits: Permits,
}

/// Count, per-connection byte, and process-wide byte bounded sender.
/// Overflow cancels the entire connection so both socket halves are reclaimed.
pub struct ConnSender<T> {
    tx: mpsc::Sender<Queued<T>>,
    bytes: Arc<Semaphore>,
    process: Arc<Semaphore>,
    cancel: CancellationToken,
}
impl<T> Clone for ConnSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            bytes: self.bytes.clone(),
            process: self.process.clone(),
            cancel: self.cancel.clone(),
        }
    }
}
impl<T> ConnSender<T> {
    pub fn same_channel(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }
}
impl<T: OutboundMessage> ConnSender<T> {
    pub fn send(&self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        if self.cancel.is_cancelled() {
            return Err(mpsc::error::SendError(value));
        }
        let permits = value.outbound_bytes().ok().and_then(|bytes| {
            let bytes = u32::try_from(bytes).ok()?;
            Some((
                self.bytes.clone().try_acquire_many_owned(bytes).ok()?,
                self.process.clone().try_acquire_many_owned(bytes).ok()?,
            ))
        });
        let Some(permits) = permits else {
            self.cancel.cancel();
            return Err(mpsc::error::SendError(value));
        };
        self.tx
            .try_send(Queued { value, permits })
            .map_err(|error| {
                self.cancel.cancel();
                mpsc::error::SendError(error.into_inner().value)
            })
    }
}

pub struct ConnReceiver<T> {
    rx: mpsc::Receiver<Queued<T>>,
    in_flight: Option<Permits>,
    cancel: CancellationToken,
}
impl<T> ConnReceiver<T> {
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
    pub async fn recv(&mut self) -> Option<T> {
        self.in_flight = None;
        let Queued { value, permits } = self.rx.recv().await?;
        self.in_flight = Some(permits);
        Some(value)
    }
    pub fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.in_flight = None;
        let Queued { value, permits } = self.rx.try_recv()?;
        self.in_flight = Some(permits);
        Ok(value)
    }
}

pub fn conn_channel<T>(capacity: usize) -> (ConnSender<T>, ConnReceiver<T>) {
    static PROCESS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    channel_with_budget(
        capacity,
        CONNECTION_BYTES,
        PROCESS
            .get_or_init(|| Arc::new(Semaphore::new(PROCESS_BYTES)))
            .clone(),
    )
}
fn channel_with_budget<T>(
    capacity: usize,
    limit: usize,
    process: Arc<Semaphore>,
) -> (ConnSender<T>, ConnReceiver<T>) {
    let (tx, rx) = mpsc::channel(capacity);
    let cancel = CancellationToken::new();
    (
        ConnSender {
            tx,
            bytes: Arc::new(Semaphore::new(limit)),
            process,
            cancel: cancel.clone(),
        },
        ConnReceiver {
            rx,
            in_flight: None,
            cancel,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Frame(usize);
    impl OutboundMessage for Frame {
        fn outbound_bytes(&self) -> Result<usize, ()> {
            Ok(self.0)
        }
    }
    #[test]
    fn bytes_include_in_flight_and_release_on_drop() {
        let process = Arc::new(Semaphore::new(10));
        let (tx, mut rx) = channel_with_budget(100, 8, process.clone());
        tx.send(Frame(6)).unwrap();
        rx.try_recv().unwrap();
        assert_eq!(process.available_permits(), 4);
        assert!(tx.send(Frame(3)).is_err());
        assert!(rx.cancellation_token().is_cancelled());
        drop(rx);
        assert_eq!(process.available_permits(), 10);
    }
    #[test]
    fn process_budget_is_shared_across_connections() {
        let process = Arc::new(Semaphore::new(10));
        let (a, rx_a) = channel_with_budget(2, 10, process.clone());
        let (b, _rx_b) = channel_with_budget(2, 10, process.clone());
        a.send(Frame(7)).unwrap();
        assert!(b.send(Frame(4)).is_err());
        drop(rx_a);
        let (c, _rx_c) = channel_with_budget(2, 10, process.clone());
        c.send(Frame(4)).unwrap();
        assert_eq!(process.available_permits(), 6);
    }
}
