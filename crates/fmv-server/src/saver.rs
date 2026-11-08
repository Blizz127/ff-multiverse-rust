//! Saving what battles pay, off the world lock and off the async executor.
//!
//! The world applies a payment to a player's mirror in memory (microseconds, under its lock) and hands
//! the SAME payment to the saver, which applies it to the stored record as an atomic read-modify-write
//! (`Store::update_character_progress`) on a blocking thread. One task drains the queue in order, so
//! payments reach the store in the order the world made them, whether or not the player is still
//! connected. What is queued is a payment (a delta), never a whole record: two results for one
//! account, or a result for an account that logged in again while it was queued, add up instead of one
//! overwriting the other.
//!
//! The queue is unbounded on purpose: a payment is a few dozen bytes, there is one per member per won
//! battle, and the world, which sends them under its lock, can neither wait for room nor drop one.
//!
//! # Shutdown
//!
//! [`Saver::shutdown`] sends a stop marker behind everything already queued, so the task finishes
//! every payment before it stops, and then waits for the task to be gone: it holds an `Arc<Store>`, and
//! the store's lock file is only released when the last one is dropped. A server that stops, or a test
//! that restarts one on the same state directory, therefore finds everything saved and the lock free.
//!
//! # Failures
//!
//! A write that fails with an I/O error is tried again twice, after 250 ms and after one second; any
//! other error (the account is gone, the record on disk is damaged or from a newer build) cannot get
//! better by waiting and is not retried. A payment that could not be saved is logged at `error` with
//! every number in it, so an operator can put it right by hand: the player saw it, the store did not
//! get it. The record on disk is never replaced by a default behind anybody's back.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fmv_core::{AccountId, GameId};
use fmv_persist::{PersistError, Store};
use fmv_protocol::{BattleReward, BattleRules};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::progress::{self, Progress};

/// How long to wait before each further attempt at a write that failed with an I/O error.
const RETRY_WAITS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];

/// One payment to save: `reward` goes to `account`, who fought as `character`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SaveJob {
    pub account: AccountId,
    /// The character that fought: a record that does not exist yet starts as this one, and the stored
    /// record follows it (see `progress`: a look changes who the account plays).
    pub character: u8,
    pub reward: BattleReward,
}

/// What the saver's queue carries.
pub(crate) enum Msg {
    Save(SaveJob),
    /// Everything before this has been saved; answer and stop.
    Stop(oneshot::Sender<()>),
}

/// The world's end of the queue. Cheap to clone, never blocks, never fails the caller.
#[derive(Clone)]
pub(crate) struct SaverHandle {
    tx: mpsc::UnboundedSender<Msg>,
}

impl SaverHandle {
    /// A queue and its receiving end (what [`Saver::spawn`] drains; tests read it directly).
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Queue a payment. `false` if the saver has stopped (the server is going down): the payment is
    /// then lost, and the caller says so in its log.
    pub(crate) fn save(&self, job: SaveJob) -> bool {
        self.tx.send(Msg::Save(job)).is_ok()
    }
}

/// The saver task and the way to stop it.
pub(crate) struct Saver {
    handle: SaverHandle,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Saver {
    /// Start draining a new queue into `store` for `game`. Needs a running tokio runtime.
    pub(crate) fn spawn(store: Arc<Store>, game: GameId, rules: Arc<dyn BattleRules>) -> Self {
        let (handle, rx) = SaverHandle::channel();
        let task = tokio::spawn(run(rx, store, game, rules));
        Self { handle, task: Mutex::new(Some(task)) }
    }

    /// The queue's sending end, for the world.
    pub(crate) fn handle(&self) -> SaverHandle {
        self.handle.clone()
    }

    /// Save everything queued so far, stop, and wait (at most `limit` for each of the two steps) until
    /// the task has gone and dropped its hold on the store. Safe to call twice.
    pub(crate) async fn shutdown(&self, limit: Duration) {
        let (done, finished) = oneshot::channel();
        if self.handle.tx.send(Msg::Stop(done)).is_ok() && tokio::time::timeout(limit, finished).await.is_err() {
            warn!(?limit, "the progress saver did not finish its queue in time; payments still queued may be lost");
        }
        let task = self.task.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(task) = task {
            match tokio::time::timeout(limit, task).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => error!(error = %e, "the progress saver task failed"),
                Err(_) => warn!(?limit, "the progress saver task is still running (it holds the account store)"),
            }
        }
    }
}

/// Apply one payment to the stored record of `job.account` (creating it from a fresh start if there is
/// none), as one atomic step, and return what is stored now. Blocking: run it off the executor.
pub(crate) fn persist(store: &Store, game: GameId, rules: &dyn BattleRules, job: &SaveJob, now: u64) -> Result<Progress, PersistError> {
    let record = store.update_character_progress(job.account, game, |current| {
        let mut progress = match current {
            Some(record) => Progress::from_record(&record),
            None => progress::fresh(rules, job.character),
        };
        // The same steps the world takes for a player that joins and then wins (`progress::resolve`,
        // `progress::apply_reward`), so the mirror and the store agree (the store is the authority; see
        // the `progress` module docs for the one case in which the mirror lags it).
        progress.character = job.character;
        progress::clamp(rules.progress_limits(), &mut progress);
        progress::apply_reward(rules, &mut progress, &job.reward);
        progress.to_record(job.account, game, now)
    })?;
    Ok(Progress::from_record(&record))
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Whether trying again could help.
fn is_transient(error: &PersistError) -> bool {
    matches!(error, PersistError::Io { .. })
}

async fn run(mut rx: mpsc::UnboundedReceiver<Msg>, store: Arc<Store>, game: GameId, rules: Arc<dyn BattleRules>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Save(job) => save_one(&store, game, &rules, job).await,
            Msg::Stop(done) => {
                let _ = done.send(());
                break;
            }
        }
    }
}

async fn save_one(store: &Arc<Store>, game: GameId, rules: &Arc<dyn BattleRules>, job: SaveJob) {
    let mut waits = RETRY_WAITS.into_iter();
    let mut attempt = 1u32;
    loop {
        let (store, rules) = (Arc::clone(store), Arc::clone(rules));
        let result = tokio::task::spawn_blocking(move || persist(&store, game, rules.as_ref(), &job, unix_now())).await;
        let failure = match result {
            Ok(Ok(saved)) => {
                // `key=value` layout: `scripts/battle-smoke.sh --progress` reads it.
                info!(
                    "progress saved account={} character={} level={} exp={} gil={} ap={}",
                    job.account.0, saved.character, saved.level, saved.exp, saved.gil, saved.ap
                );
                return;
            }
            Ok(Err(failure)) => failure,
            Err(e) => {
                error!(error = %e, ?job, "the progress write task failed; the payment is lost");
                return;
            }
        };
        match waits.next() {
            Some(wait) if is_transient(&failure) => {
                warn!(account = job.account.0, attempt, error = %failure, "could not save character progress; trying again");
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            _ => {
                error!(
                    "character progress NOT SAVED (the player has it, the store does not): account={} character={} exp={} gil={} ap={} error={failure}",
                    job.account.0, job.character, job.reward.exp, job.reward.gil, job.reward.ap
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_protocol::SyntheticRules;

    const NOW: u64 = 1_000;

    /// `SyntheticRules`: level 5 starts at 40 experience, level 6 at 60, 7 at 84, 8 at 112.
    fn rules() -> Arc<dyn BattleRules> {
        Arc::new(SyntheticRules::default())
    }

    fn store() -> (tempfile::TempDir, Arc<Store>, AccountId) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        let (account, _token) = store.create_account("Player", NOW).unwrap();
        (dir, store, account)
    }

    fn job(account: AccountId, character: u8, exp: u32, gil: u32, ap: u32) -> SaveJob {
        SaveJob { account, character, reward: BattleReward { account, exp, gil, ap } }
    }

    fn stored(store: &Store, account: AccountId) -> Option<Progress> {
        store.get_character_progress(account, GameId::Ff7).unwrap().map(|r| Progress::from_record(&r))
    }

    #[test]
    fn a_payment_creates_the_record_from_a_fresh_start_and_later_ones_add_up() {
        let (_dir, store, account) = store();
        let rules = rules();
        assert_eq!(stored(&store, account), None, "nothing is written until the first payment");

        // A fresh Cloud is level 5 with 40 experience; 12 more is 52, no level.
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 12, 20, 2), NOW).unwrap();
        assert_eq!(saved, Progress { character: 0, level: 5, exp: 52, ap: 2, gil: 20 });
        assert_eq!(stored(&store, account), Some(saved));
        let record = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        assert_eq!((record.updated_at, record.account_id, record.game), (NOW, account, GameId::Ff7));

        // 52 + 24 = 76: level 6 (60). The next payment starts from what is stored.
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 24, 40, 4), NOW + 1).unwrap();
        assert_eq!(saved, Progress { character: 0, level: 6, exp: 76, ap: 6, gil: 60 });
        assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap().unwrap().updated_at, NOW + 1);
    }

    #[test]
    fn the_stored_record_follows_the_character_that_fought_and_keeps_what_was_earned() {
        let (_dir, store, account) = store();
        let rules = rules();
        persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 30, 10, 1), NOW).unwrap();
        // The account's look changed to Tifa (2): she carries the level and the totals over.
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 2, 1, 0, 0), NOW).unwrap();
        assert_eq!(saved, Progress { character: 2, level: 6, exp: 71, ap: 1, gil: 10 }, "40 + 30 + 1 is level 6");
    }

    #[test]
    fn the_games_caps_apply_in_the_store_too_and_a_record_outside_them_is_brought_inside() {
        let (_dir, store, account) = store();
        let rules = rules();
        let limits = rules.progress_limits();
        // A hand-edited record far outside the caps (the store has no opinion on them).
        let wild = Progress { character: 0, level: 250, exp: u32::MAX, ap: u32::MAX, gil: u32::MAX };
        store.put_character_progress(&wild.to_record(account, GameId::Ff7, NOW)).unwrap();
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 5, 5, 5), NOW).unwrap();
        assert_eq!(
            saved,
            Progress { character: 0, level: limits.max_level, exp: limits.max_exp, ap: limits.max_ap, gil: limits.max_gil }
        );
    }

    #[test]
    fn a_payment_for_an_account_that_does_not_exist_is_an_error_and_writes_nothing() {
        let (dir, store, _account) = store();
        let ghost = AccountId(77);
        let err = persist(&store, GameId::Ff7, rules().as_ref(), &job(ghost, 0, 1, 1, 1), NOW).unwrap_err();
        assert!(matches!(err, PersistError::UnknownAccount(_)), "{err}");
        assert!(!is_transient(&err), "waiting does not create an account");
        assert!(!dir.path().join("state").join("progression").join("77").exists());
    }

    /// The mirror (what the world applies as battles are won) and the store (what the saver applies
    /// to the record) are two computations of one thing. They start equal and see the same payments,
    /// so they must end equal, for any payments in any order, at every step.
    #[test]
    fn the_mirror_and_the_store_agree_after_every_payment() {
        let (_dir, store, account) = store();
        let rules = rules();
        let mut mirror = progress::resolve(rules.as_ref(), progress::ProgressSeed { character: 3, stored: None });
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |n: u32| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % u64::from(n)) as u32
        };
        for step in 0..300 {
            let reward = BattleReward { account, exp: next(90), gil: next(40), ap: next(6) };
            let leveled = progress::apply_reward(rules.as_ref(), &mut mirror, &reward);
            let saved = persist(&store, GameId::Ff7, rules.as_ref(), &SaveJob { account, character: 3, reward }, NOW).unwrap();
            assert_eq!(saved, mirror, "step {step} (leveled up in the mirror: {leveled})");
        }
        assert!(mirror.level > 5, "300 payments level a character up: {mirror:?}");
        assert_eq!(stored(&store, account), Some(mirror));
    }

    /// Everything queued before the stop marker is saved, in order, before `shutdown` returns, and
    /// the saver no longer holds the store then: the lock file is free for the next server.
    #[tokio::test]
    async fn shutdown_saves_everything_queued_in_order_and_lets_go_of_the_store() {
        let (dir, store, account) = store();
        let root = dir.path().join("state");
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, rules());
        let handle = saver.handle();
        for i in 1..=20u32 {
            assert!(handle.save(job(account, 0, i, 1, 1)));
        }
        saver.shutdown(Duration::from_secs(20)).await;

        // 40 + (1 + ... + 20) = 250 experience (level 11: 220, level 12: 264), 20 gil, 20 AP.
        let p = stored(&store, account).expect("saved");
        assert_eq!((p.exp, p.gil, p.ap), (250, 20, 20));
        assert_eq!(p.level, 11);
        // The saver is gone: its handle refuses new work, and the test holds the only reference to the store.
        assert!(!handle.save(job(account, 0, 1, 1, 1)), "a stopped saver takes nothing more");
        assert_eq!(Arc::strong_count(&store), 1);
        drop(store);
        let reopened = Store::open(&root).expect("the lock is free again");
        assert_eq!(reopened.get_character_progress(account, GameId::Ff7).unwrap().unwrap().exp, 250);
        // Stopping again is harmless.
        saver.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn a_payment_that_cannot_be_saved_does_not_hold_up_the_ones_behind_it() {
        let (_dir, store, account) = store();
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, rules());
        let handle = saver.handle();
        assert!(handle.save(job(AccountId(404), 0, 9, 9, 9))); // no such account: refused, not retried
        assert!(handle.save(job(account, 0, 5, 6, 7)));
        saver.shutdown(Duration::from_secs(20)).await;
        let p = stored(&store, account).unwrap();
        assert_eq!((p.exp, p.gil, p.ap), (45, 6, 7));
        let ghost = store.get_character_progress(AccountId(404), GameId::Ff7);
        assert!(matches!(ghost, Err(PersistError::UnknownAccount(_))), "nothing was made for the ghost: {ghost:?}");
    }

    #[test]
    fn the_queue_a_test_reads_carries_what_the_world_sends() {
        let (handle, mut rx) = SaverHandle::channel();
        let a = AccountId(5);
        assert!(handle.save(job(a, 2, 1, 2, 3)));
        match rx.try_recv() {
            Ok(Msg::Save(j)) => assert_eq!(j, job(a, 2, 1, 2, 3)),
            _ => panic!("the payment was not queued"),
        }
        assert!(rx.try_recv().is_err());
        drop(rx);
        assert!(!handle.save(job(a, 2, 1, 2, 3)), "nobody is listening any more");
    }
}
