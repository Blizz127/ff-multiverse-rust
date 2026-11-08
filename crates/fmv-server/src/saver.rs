//! Saving what battles pay and what players use, off the world lock and off the async executor.
//!
//! The world applies a payment to a player's mirror in memory (microseconds, under its lock) and hands
//! the SAME payment to the saver, which applies it to the stored record as an atomic read-modify-write
//! (`Store::update_character_progress`) on a blocking thread. One task drains the queue in order, so
//! payments reach the store in the order the world made them, whether or not the player is still
//! connected. What is queued is a payment (a delta), never a whole record: two results for one
//! account, or a result for an account that logged in again while it was queued, add up instead of one
//! overwriting the other. A payment is a won battle's experience, gil, AP and dropped items
//! ([`SavedChange::Reward`]), one item an account used in a battle ([`SavedChange::UsedItem`]) or the limit
//! gauge a character ended a battle with ([`SavedChange::Limit`]: the one change that is a value, not a delta,
//! which is safe because an account is in one battle at a time and the queue keeps the order).
//!
//! # The inventory in the record
//!
//! The stored inventory is part of the same record, so every write carries it over: the record is built
//! from the numbers ([`Progress::to_record`], which has none) and then given the inventory the stored
//! record had, changed by the payment. A record with no inventory (`None`: an account of an older build,
//! or one that has never held anything) stays without one unless the payment is about items; the first
//! payment that is gives it the game's starting stock with the change applied (`inventory`: the same
//! rule the world's mirror follows, which is what keeps the two equal).
//!
//! The queue is unbounded on purpose: a payment is a few dozen bytes, there is one per member per won
//! battle (and one per item used), and the world, which sends them under its lock, can neither wait for
//! room nor drop one.
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

use crate::inventory::{Change, Inventory};
use crate::progress::{self, Progress};

/// How long to wait before each further attempt at a write that failed with an I/O error.
const RETRY_WAITS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];

/// One payment to save: a change to the record of `account`, who fought as `character`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SaveJob {
    pub account: AccountId,
    /// The character that fought: a record that does not exist yet starts as this one, and the stored
    /// record follows it (see `progress`: a look changes who the account plays).
    pub character: u8,
    pub change: SavedChange,
}

/// What a [`SaveJob`] changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SavedChange {
    /// What a won battle paid: experience, gil, AP and the items it dropped to this account.
    Reward(BattleReward),
    /// One item the account used in a battle (taken out of the stored inventory).
    UsedItem(u16),
    /// The limit gauge the account's character ended a battle with (protocol v7; any outcome, an escape
    /// included). It is set, not added: the world only queues it when it differs from the one the battle
    /// started with, so a gauge that did not move writes nothing. An account with no stored record gets
    /// one from a fresh start (it is the one change that can create a record before a first win).
    Limit(u16),
}

impl SaveJob {
    pub(crate) fn reward(account: AccountId, character: u8, reward: BattleReward) -> Self {
        Self { account, character, change: SavedChange::Reward(reward) }
    }

    pub(crate) fn used_item(account: AccountId, character: u8, item: u16) -> Self {
        Self { account, character, change: SavedChange::UsedItem(item) }
    }

    pub(crate) fn limit(account: AccountId, character: u8, limit: u16) -> Self {
        Self { account, character, change: SavedChange::Limit(limit) }
    }

    /// The payment of a reward job.
    #[cfg(test)]
    pub(crate) fn paid(&self) -> Option<&BattleReward> {
        match &self.change {
            SavedChange::Reward(reward) => Some(reward),
            SavedChange::UsedItem(_) | SavedChange::Limit(_) => None,
        }
    }
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

/// What [`persist`] stored: the progress as it is now and how many items the inventory holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Saved {
    pub progress: Progress,
    pub items: u32,
}

/// Apply one payment to the stored record of `job.account` (creating it from a fresh start if there is
/// none), as one atomic step, and return what is stored now. Blocking: run it off the executor.
pub(crate) fn persist(store: &Store, game: GameId, rules: &dyn BattleRules, job: &SaveJob, now: u64) -> Result<Saved, PersistError> {
    let mut items = 0u32;
    let record = store.update_character_progress(job.account, game, |current| {
        let (mut progress, stored) = match current {
            Some(record) => (Progress::from_record(&record), record.inventory),
            None => (progress::fresh(rules, job.character), None),
        };
        // The same steps the world takes for a player that joins and then wins (`progress::resolve`,
        // `progress::apply_reward`, `Inventory::resolve`, `Inventory::apply`), so the mirror and the store
        // agree (the store is the authority; see the `progress` module docs for the one case in which the
        // mirror lags it).
        progress.character = job.character;
        progress::clamp(rules.progress_limits(), &mut progress);
        let max_stack = rules.max_item_stack();
        let mut inventory = Inventory::resolve(rules, stored.as_deref());
        let about_items = match &job.change {
            SavedChange::Reward(reward) => {
                progress::apply_reward(rules, &mut progress, reward);
                inventory.apply(&Change::Found(reward.items.clone()), max_stack);
                !reward.items.is_empty()
            }
            SavedChange::UsedItem(item) => {
                inventory.apply(&Change::Used(*item), max_stack);
                true
            }
            SavedChange::Limit(value) => {
                progress.limit = (*value).min(rules.progress_limits().max_limit);
                false
            }
        };
        items = inventory.total();
        let mut record = progress.to_record(job.account, game, now);
        // An inventory that was stored stays stored; one that was not appears with the first change to it.
        record.inventory = (stored.is_some() || about_items).then(|| inventory.entries().to_vec());
        record
    })?;
    Ok(Saved { progress: Progress::from_record(&record), items })
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
        let (store, rules, attempt_job) = (Arc::clone(store), Arc::clone(rules), job.clone());
        let result = tokio::task::spawn_blocking(move || persist(&store, game, rules.as_ref(), &attempt_job, unix_now())).await;
        let failure = match result {
            Ok(Ok(saved)) => {
                // `key=value` layout: `scripts/battle-smoke.sh --progress` reads it (new keys at the end), and
                // counts one `progress saved` line per member per won battle, so an item used (a change
                // to the inventory alone) has a line of its own.
                match &job.change {
                    SavedChange::Reward(_) => info!(
                        "progress saved account={} character={} level={} exp={} gil={} ap={} items={}",
                        job.account.0, saved.progress.character, saved.progress.level, saved.progress.exp, saved.progress.gil, saved.progress.ap, saved.items
                    ),
                    SavedChange::UsedItem(item) => {
                        info!("item use saved account={} character={} item={item} items={}", job.account.0, job.character, saved.items)
                    }
                    // Not a `progress saved` line: the smoke test counts those, one per member per won battle.
                    SavedChange::Limit(_) => info!(
                        "limit saved account={} character={} limit={} level={} exp={}",
                        job.account.0, saved.progress.character, saved.progress.limit, saved.progress.level, saved.progress.exp
                    ),
                }
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
                    "character progress NOT SAVED (the player has it, the store does not): account={} character={} change={:?} error={failure}",
                    job.account.0, job.character, job.change
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
        SaveJob::reward(account, character, BattleReward::new(account, exp, gil, ap))
    }

    /// What [`persist`] left of the progress (most tests are about the numbers only).
    fn persist_progress(store: &Store, game: GameId, rules: &dyn BattleRules, job: &SaveJob, now: u64) -> Result<Progress, PersistError> {
        persist(store, game, rules, job, now).map(|saved| saved.progress)
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
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 12, 20, 2), NOW).unwrap();
        assert_eq!(saved, Progress { character: 0, level: 5, exp: 52, ap: 2, gil: 20, limit: 0 });
        assert_eq!(stored(&store, account), Some(saved));
        let record = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        assert_eq!((record.updated_at, record.account_id, record.game), (NOW, account, GameId::Ff7));

        // 52 + 24 = 76: level 6 (60). The next payment starts from what is stored.
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 24, 40, 4), NOW + 1).unwrap();
        assert_eq!(saved, Progress { character: 0, level: 6, exp: 76, ap: 6, gil: 60, limit: 0 });
        assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap().unwrap().updated_at, NOW + 1);
    }

    #[test]
    fn the_stored_record_follows_the_character_that_fought_and_keeps_what_was_earned() {
        let (_dir, store, account) = store();
        let rules = rules();
        persist_progress(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 30, 10, 1), NOW).unwrap();
        // The account's look changed to Tifa (2): she carries the level and the totals over.
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &job(account, 2, 1, 0, 0), NOW).unwrap();
        assert_eq!(saved, Progress { character: 2, level: 6, exp: 71, ap: 1, gil: 10, limit: 0 }, "40 + 30 + 1 is level 6");
    }

    #[test]
    fn the_games_caps_apply_in_the_store_too_and_a_record_outside_them_is_brought_inside() {
        let (_dir, store, account) = store();
        let rules = rules();
        let limits = rules.progress_limits();
        // A hand-edited record far outside the caps (the store has no opinion on them).
        let wild = Progress { character: 0, level: 250, exp: u32::MAX, ap: u32::MAX, gil: u32::MAX, limit: u16::MAX };
        store.put_character_progress(&wild.to_record(account, GameId::Ff7, NOW)).unwrap();
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 5, 5, 5), NOW).unwrap();
        assert_eq!(
            saved,
            Progress { character: 0, level: limits.max_level, exp: limits.max_exp, ap: limits.max_ap, gil: limits.max_gil, limit: limits.max_limit }
        );
    }

    // ---- the limit gauge in the record ----

    #[test]
    fn a_gauge_is_set_not_added_and_a_payment_after_it_keeps_it() {
        let (_dir, store, account) = store();
        let rules = rules();
        // Nothing stored yet, and a battle that was escaped (so paid nothing) ended with a gauge worth
        // keeping: the record is made from a fresh start with the gauge in it.
        assert_eq!(stored(&store, account), None);
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &SaveJob::limit(account, 0, 137), NOW).unwrap();
        assert_eq!(saved.progress, Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 137 }, "a fresh Cloud, nothing earned, 137 of 255");
        assert_eq!(saved.items, 3 + 1 + 1, "the inventory is told as the starting stock, and nothing was written for it");
        assert_eq!(stored_inventory(&store, account), None, "a gauge is not about items: the starting stock is not written");
        // A second battle's end value replaces it, whatever it was (this is no payment).
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &SaveJob::limit(account, 0, 20), NOW + 1).unwrap();
        assert_eq!(saved.limit, 20);
        // A win pays and leaves the gauge as it is; a gauge written after it leaves the payment as it is.
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 12, 20, 2), NOW + 2).unwrap();
        assert_eq!(saved, Progress { character: 0, level: 5, exp: 52, ap: 2, gil: 20, limit: 20 });
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &SaveJob::limit(account, 0, 0), NOW + 3).unwrap();
        assert_eq!(saved, Progress { character: 0, level: 5, exp: 52, ap: 2, gil: 20, limit: 0 });
        assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap().unwrap().updated_at, NOW + 3);
    }

    #[test]
    fn the_inventory_survives_a_gauge_and_a_gauge_above_the_cap_is_brought_inside() {
        let (_dir, store, account) = store();
        let rules = rules();
        // A record that holds an inventory (a drop was the first change).
        persist(&store, GameId::Ff7, rules.as_ref(), &drop_job(account, &[e(0, 2)]), NOW).unwrap();
        let before = stored_inventory(&store, account).expect("an inventory");
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &SaveJob::limit(account, 0, 200), NOW + 1).unwrap();
        assert_eq!(saved.limit, 200);
        assert_eq!(stored_inventory(&store, account), Some(before), "writing the gauge touches no item");
        // More than the game's cap is not kept.
        let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &SaveJob::limit(account, 0, u16::MAX), NOW + 2).unwrap();
        assert_eq!(saved.limit, rules.progress_limits().max_limit);
        // A record that was never given a gauge (what a v6 record reads as: see `fmv-persist`'s tests of
        // the missing key) is an empty one in the mirror.
        assert_eq!(Progress::from_record(&fmv_persist::CharacterProgress::new(account, GameId::Ff7, 0, 5, NOW)).limit, 0);
    }

    #[test]
    fn a_payment_for_an_account_that_does_not_exist_is_an_error_and_writes_nothing() {
        let (dir, store, _account) = store();
        let ghost = AccountId(77);
        let err = persist_progress(&store, GameId::Ff7, rules().as_ref(), &job(ghost, 0, 1, 1, 1), NOW).unwrap_err();
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
        let mut mirror = progress::resolve(rules.as_ref(), &progress::ProgressSeed::of(3, None));
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |n: u32| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % u64::from(n)) as u32
        };
        for step in 0..300 {
            let reward = BattleReward::new(account, next(90), next(40), next(6));
            let leveled = progress::apply_reward(rules.as_ref(), &mut mirror, &reward);
            let saved = persist_progress(&store, GameId::Ff7, rules.as_ref(), &SaveJob::reward(account, 3, reward), NOW).unwrap();
            assert_eq!(saved, mirror, "step {step} (leveled up in the mirror: {leveled})");
        }
        assert!(mirror.level > 5, "300 payments level a character up: {mirror:?}");
        assert_eq!(stored(&store, account), Some(mirror));
    }

    // ---- the inventory in the record ----

    fn e(item: u16, count: u16) -> fmv_core::InventoryEntry {
        fmv_core::InventoryEntry::new(item, count)
    }

    fn stored_inventory(store: &Store, account: AccountId) -> Option<Vec<fmv_core::InventoryEntry>> {
        store.get_character_progress(account, GameId::Ff7).unwrap().and_then(|r| r.inventory)
    }

    fn drop_job(account: AccountId, items: &[fmv_core::InventoryEntry]) -> SaveJob {
        SaveJob::reward(account, 0, BattleReward { items: items.to_vec(), ..BattleReward::new(account, 1, 0, 0) })
    }

    /// The starting stock is written into the record once, by the first change that is about items, and
    /// from then on the record is the inventory.
    #[test]
    fn the_first_item_change_writes_the_starting_stock_with_the_change_applied_once() {
        let (_dir, store, account) = store();
        let rules = rules();
        // A payment of numbers only writes no inventory: the account has none to speak of yet.
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 12, 20, 2), NOW).unwrap();
        assert_eq!(stored_inventory(&store, account), None);
        assert_eq!(saved.items, 5, "the mirror's stock (3 Potions, an Ether, a Phoenix Down) is what it counts");
        // Using a Potion writes the stock minus one: Potion x2, Ether x1, Phoenix Down x1.
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &SaveJob::used_item(account, 0, 0), NOW).unwrap();
        assert_eq!(stored_inventory(&store, account), Some(vec![e(0, 2), e(3, 1), e(7, 1)]));
        assert_eq!(saved.items, 4);
        // The numbers of the first payment are still there (an item job pays nothing).
        assert_eq!(stored(&store, account), Some(Progress { character: 0, level: 5, exp: 52, ap: 2, gil: 20, limit: 0 }));
        // A later payment of numbers keeps the inventory exactly as it is (it is stored now).
        persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 1, 1, 1), NOW + 1).unwrap();
        assert_eq!(stored_inventory(&store, account), Some(vec![e(0, 2), e(3, 1), e(7, 1)]));
        // Drops add (a new item, and more of one held; a stack stops at the cap).
        persist(&store, GameId::Ff7, rules.as_ref(), &drop_job(account, &[e(1, 2), e(0, 200)]), NOW + 2).unwrap();
        assert_eq!(stored_inventory(&store, account), Some(vec![e(0, 99), e(1, 2), e(3, 1), e(7, 1)]));
        // The last of an item takes the stack away; using what is not there changes nothing.
        persist(&store, GameId::Ff7, rules.as_ref(), &SaveJob::used_item(account, 0, 7), NOW + 3).unwrap();
        persist(&store, GameId::Ff7, rules.as_ref(), &SaveJob::used_item(account, 0, 7), NOW + 4).unwrap();
        assert_eq!(stored_inventory(&store, account), Some(vec![e(0, 99), e(1, 2), e(3, 1)]));
    }

    /// The old v5 records have no inventory: a battle's payment must neither invent one nor lose one that
    /// exists (the record is replaced as a whole on every payment).
    #[test]
    fn a_payment_neither_invents_an_inventory_nor_loses_the_one_the_record_has() {
        let (_dir, store, account) = store();
        let rules = rules();
        // A record of the previous version: numbers, no inventory.
        let old = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 0 }.to_record(account, GameId::Ff7, NOW);
        assert_eq!(old.inventory, None);
        store.put_character_progress(&old).unwrap();
        persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 12, 20, 2), NOW).unwrap();
        assert_eq!(stored_inventory(&store, account), None, "still a record of the old shape");

        // A record with an inventory, even an empty one, keeps it through payments.
        let mut with = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        with.inventory = Some(vec![]);
        store.put_character_progress(&with).unwrap();
        persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 5, 5, 5), NOW + 1).unwrap();
        assert_eq!(stored_inventory(&store, account), Some(vec![]), "an empty inventory stays empty: the stock is given once");
        let mut with = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        with.inventory = Some(vec![e(2, 7), e(9, 1)]);
        store.put_character_progress(&with).unwrap();
        for i in 0..3 {
            persist(&store, GameId::Ff7, rules.as_ref(), &job(account, 0, 5, 5, 5), NOW + 2 + i).unwrap();
        }
        assert_eq!(stored_inventory(&store, account), Some(vec![e(2, 7), e(9, 1)]));
    }

    /// What is stored is not trusted: a hostile inventory is brought inside the rules by the first change
    /// (the store only checks what is true of every game).
    #[test]
    fn a_stored_inventory_outside_the_rules_is_brought_inside_by_the_next_change() {
        let (_dir, store, account) = store();
        let rules = rules();
        let mut wild = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 0 }.to_record(account, GameId::Ff7, NOW);
        wild.inventory = Some(vec![e(5, 60_000), e(2, 3)]);
        store.put_character_progress(&wild).unwrap();
        persist(&store, GameId::Ff7, rules.as_ref(), &SaveJob::used_item(account, 0, 2), NOW).unwrap();
        assert_eq!(stored_inventory(&store, account), Some(vec![e(2, 2), e(5, 99)]));
    }

    /// The mirror (what the world keeps as items are used and found) and the store (what the saver keeps)
    /// are two computations of one thing, like the numbers: for any changes in any order they agree.
    #[test]
    fn the_mirror_and_the_store_agree_on_the_inventory_after_every_change() {
        let (_dir, store, account) = store();
        let rules = rules();
        let max_stack = rules.max_item_stack();
        let mut mirror = Inventory::resolve(rules.as_ref(), None);
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for step in 0..400 {
            let job = if next(3) == 0 {
                let item = next(12) as u16;
                mirror.apply(&Change::Used(item), max_stack);
                SaveJob::used_item(account, 0, item)
            } else {
                let found: Vec<fmv_core::InventoryEntry> = (0..next(4)).map(|_| e(next(12) as u16, 1 + next(40) as u16)).collect();
                let reward = BattleReward { items: found.clone(), ..BattleReward::new(account, next(20) as u32, 0, 0) };
                mirror.apply(&Change::Found(found), max_stack);
                SaveJob::reward(account, 0, reward)
            };
            let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job, NOW).unwrap();
            assert_eq!(saved.items, mirror.total(), "step {step}");
            // The stored inventory, once there is one, is the mirror (before that, the stock is).
            let stored = stored_inventory(&store, account);
            assert_eq!(Inventory::resolve(rules.as_ref(), stored.as_deref()), mirror, "step {step}");
        }
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
