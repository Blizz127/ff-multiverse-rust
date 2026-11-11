//! Saving where players stand, off the world lock and off the async executor (MMO-style saving: see
//! `world::resume`).
//!
//! A spot is a VALUE, not a payment: only the newest one of an account matters. The world queues one
//! whenever a player arrives somewhere, every few seconds while it walks, and when it leaves; this task
//! keeps the newest of each account and writes it with [`Store::put_location`] on a blocking thread.
//! So a burst of spots for one account is one file write, and nothing here can grow without bound.
//!
//! It is a queue of its own, separate from `saver` (what battles pay and the story): a spot never waits
//! behind a scene transaction and a scene never waits behind a spot. A login judges a spot by the story
//! saved with it (`SavedLocation::story`), so the two queues need no order between them.
//!
//! # Flush
//!
//! [`LocationSaverHandle::flush`] answers once everything queued before it is written. A connection that ends waits
//! for it (briefly) before it closes, so a player that logs out and straight back in finds the spot it left.
//!
//! # Shutdown
//!
//! [`LocationSaver::shutdown`] sends a stop marker behind everything already queued: the task writes
//! every spot it holds before it stops, then lets go of the store. The server calls it after every
//! connection has closed (each leaving player has queued its last spot by then).
//!
//! # Failures
//!
//! A write that fails is logged and dropped: the player's next spot (seconds later, or at its next
//! arrival) is written instead. A spot for an account that was deleted meanwhile is refused by the
//! store (it never brings the account's directory back) and is dropped quietly.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fmv_persist::{PersistError, SavedLocation, Store};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// What the queue carries.
pub(crate) enum Msg {
    Spot(SavedLocation),
    /// Answer once everything before this has been written.
    Flush(oneshot::Sender<()>),
    /// Everything before this has been written; answer and stop.
    Stop(oneshot::Sender<()>),
}

/// What ends a batch: nothing more was queued, a flush to answer, or the stop.
enum Marker {
    None,
    Flush(oneshot::Sender<()>),
    Stop(oneshot::Sender<()>),
}

/// The world's end of the queue. Cheap to clone, never blocks, never fails the caller.
#[derive(Clone)]
pub(crate) struct LocationSaverHandle {
    tx: mpsc::UnboundedSender<Msg>,
}

impl LocationSaverHandle {
    /// A queue and its receiving end (what [`LocationSaver::spawn`] drains; tests read it directly).
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Queue where a player stands now. `false` if the saver has stopped (the server is going down).
    pub(crate) fn save(&self, spot: SavedLocation) -> bool {
        self.tx.send(Msg::Spot(spot)).is_ok()
    }

    /// Resolves once every spot queued before this call is written (`None`: the saver has stopped).
    pub(crate) fn flush(&self) -> Option<oneshot::Receiver<()>> {
        let (done, flushed) = oneshot::channel();
        self.tx.send(Msg::Flush(done)).ok().map(|()| flushed)
    }
}

/// The saver task and the way to stop it.
pub(crate) struct LocationSaver {
    handle: LocationSaverHandle,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl LocationSaver {
    /// Start draining a new queue into `store`. Needs a running tokio runtime.
    pub(crate) fn spawn(store: Arc<Store>) -> Self {
        let (handle, rx) = LocationSaverHandle::channel();
        let task = tokio::spawn(run(rx, store));
        Self { handle, task: Mutex::new(Some(task)) }
    }

    pub(crate) fn handle(&self) -> LocationSaverHandle {
        self.handle.clone()
    }

    /// See [`LocationSaverHandle::flush`].
    pub(crate) fn flush(&self) -> Option<oneshot::Receiver<()>> {
        self.handle.flush()
    }

    /// Write everything queued so far, stop, and wait (at most `limit` for each of the two steps) until
    /// the task has gone and dropped its hold on the store. Safe to call twice.
    pub(crate) async fn shutdown(&self, limit: Duration) {
        let (done, finished) = oneshot::channel();
        if self.handle.tx.send(Msg::Stop(done)).is_ok() && tokio::time::timeout(limit, finished).await.is_err() {
            warn!(?limit, "the location saver did not finish its queue in time; the newest spots may be lost");
        }
        let task = self.task.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(task) = task {
            if tokio::time::timeout(limit, task).await.is_err() {
                warn!(?limit, "the location saver task is still running (it holds the account store)");
            }
        }
    }
}

/// Seconds since the epoch, for `SavedLocation::updated_at`.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Take everything that is queued now (at least one message, waiting for it), keeping the newest spot
/// of each account, up to the first flush or stop marker (what came before a marker is written before it is answered).
async fn next_batch(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Option<(BTreeMap<u64, SavedLocation>, Marker)> {
    let mut spots = BTreeMap::new();
    let mut next = Some(rx.recv().await?);
    while let Some(msg) = next.take() {
        match msg {
            Msg::Spot(spot) => {
                spots.insert(spot.account_id.0, spot);
            }
            Msg::Flush(done) => return Some((spots, Marker::Flush(done))),
            Msg::Stop(done) => return Some((spots, Marker::Stop(done))),
        }
        next = rx.try_recv().ok();
    }
    Some((spots, Marker::None))
}

async fn run(mut rx: mpsc::UnboundedReceiver<Msg>, store: Arc<Store>) {
    while let Some((spots, marker)) = next_batch(&mut rx).await {
        if !spots.is_empty() {
            let store = Arc::clone(&store);
            let written = tokio::task::spawn_blocking(move || write_all(&store, spots)).await;
            if let Err(e) = written {
                warn!(error = %e, "the location writer failed; the newest spots of this batch are lost");
            }
        }
        match marker {
            Marker::None => {}
            Marker::Flush(done) => {
                let _ = done.send(());
            }
            Marker::Stop(done) => {
                let _ = done.send(());
                return;
            }
        }
    }
}

/// Write each spot (blocking). Failures are logged, never retried: a newer spot follows soon.
fn write_all(store: &Store, spots: BTreeMap<u64, SavedLocation>) {
    let now = now_secs();
    for (_, mut spot) in spots {
        spot.updated_at = now;
        match store.put_location(&spot) {
            Ok(()) => debug!(account = spot.account_id.0, area = %spot.area, resumable = spot.resumable, "spot saved"),
            Err(PersistError::UnknownAccount(account)) => {
                info!(account = account.0, "a spot for an account that is gone was dropped (it was deleted while the spot was queued)")
            }
            Err(e) => warn!(account = spot.account_id.0, area = %spot.area, error = %e, "a spot could not be saved; the next one will be"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{AccountId, GameId};

    fn spot(account: AccountId, x: f32) -> SavedLocation {
        SavedLocation::new(account, GameId::Ff7, "md1_1", [x, 2.0, 3.0], 0.5, vec![1, 8, 0], true, 0)
    }

    #[tokio::test]
    async fn the_newest_spot_of_each_account_is_written_and_shutdown_writes_what_is_queued() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let a = store.register("A", [1, 2, 3], "lean", None, 0).unwrap().account_id;
        let b = store.register("B", [1, 2, 3], "lean", None, 0).unwrap().account_id;
        let saver = LocationSaver::spawn(Arc::clone(&store));
        let handle = saver.handle();
        for x in [1.0, 2.0, 3.0] {
            assert!(handle.save(spot(a, x)));
        }
        assert!(handle.save(spot(b, 9.0)));
        // A flush answers once what came before it is on disk.
        handle.flush().expect("running").await.unwrap();
        assert_eq!(store.get_location(a, GameId::Ff7).unwrap().map(|s| s.pos), Some([3.0, 2.0, 3.0]), "written before the flush answered");
        // A spot for an account that is gone is dropped, and creates nothing.
        assert!(handle.save(spot(AccountId(77), 1.0)));
        saver.shutdown(Duration::from_secs(5)).await;
        assert!(!handle.save(spot(a, 4.0)), "a stopped saver says so");
        assert!(handle.flush().is_none());
        let saved_a = store.get_location(a, GameId::Ff7).unwrap().expect("A's spot");
        assert_eq!(saved_a.pos, [3.0, 2.0, 3.0], "the newest spot wins");
        assert!(saved_a.updated_at > 0, "the write is stamped");
        assert_eq!(store.get_location(b, GameId::Ff7).unwrap().map(|s| s.pos), Some([9.0, 2.0, 3.0]));
        assert!(!dir.path().join("progression").join("77").exists());
    }
}
