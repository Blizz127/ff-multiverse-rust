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
//!
//! Scene transactions use a different contract: [`SceneSaveJob`] is queued behind earlier writes,
//! checks an exact story predecessor, and commits story and resource changes together. Its receipt
//! survives reconnects and later rewards. The caller receives a durable acknowledgement before it
//! may publish progress or release a held scene. An uncertain error is never retried here; the store
//! must recover first. The scene world uses this acknowledgement to commit and release.
//!
//! [`SceneBatchJob`] extends this contract to a suspended scene's frozen battle roster. The world
//! reserves its accounts, requests an ordered snapshot, then commits the checkpoint before combat.
//! A terminal battle commits accepted item costs, rewards, gauges and continuation for every member
//! together. These bounded jobs are larger than normal payments, so the world permits only one
//! pending operation per reserved roster. Item use during such a fight is staged until settlement.
//!
//! [`StoryCasJob`] replaces an ordinary story only against its exact predecessor, without a
//! permanent receipt for repeatable field changes. [`StoryBattleJob`] settles an ordinary story
//! battle's rewards, gauges and owner story together. Such battles keep ordinary accepted item
//! saves; their terminal snapshot follows those saves in this FIFO and never debits them again.
//! A checked baseline before combat ensures every member has room for the outcome receipt.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, GameId};
use fmv_persist::{PersistError, Store};
use fmv_protocol::{BattleReward, BattleRules};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::inventory::{Change, Inventory};
use crate::progress::{self, Progress};

mod continuation;
pub(crate) use continuation::{persist_scene_batch, preview_scene_settlement, SceneBatchJob, SceneBatchMember, SceneBattleSettlement, MAX_SCENE_BATTLE_ITEM_USES};
mod ordinary;
pub(crate) use ordinary::{persist_story_battle, persist_story_cas, StoryBattleJob, StoryBattleMember, StoryCasJob};

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

/// One story save: the progression blob an account has now (the world's mirror, after a story point
/// advanced it). Unlike a payment it is a value, not a delta; what keeps it safe is that the write is a
/// read-modify-write with the adapter's own merge (`GameAdapter::merge_progression`: a story only moves
/// forward), so two saves arriving in any order, or one arriving after a later login saved more, leave
/// the furthest story stored, and a record the adapter cannot read is never overwritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoryJob {
    pub account: AccountId,
    pub progress: Vec<u8>,
}

/// A scene result computed by the authoritative game interpreter. The saver checks the exact
/// story and gil predecessors after all earlier queued rewards have reached storage.
/// The world must keep participants held until the acknowledgement succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SceneSaveJob {
    pub account: AccountId,
    pub scene_key: String,
    pub expected_story: Vec<u8>,
    pub next_story: Vec<u8>,
    /// The authoritative interpreter evaluated the scene against this resource snapshot.
    pub expected_gil: u32,
    /// Exact authored result (including the game's saturation rules); the saver never invents arithmetic.
    pub next_gil: u32,
}

/// A coherent stored pair after the scene has durably committed. On a duplicate it may contain
/// later rewards or progress; callers must use these values, not replay the old after-image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SceneSaved {
    pub commit: fmv_persist::SceneCommit,
    pub pair: fmv_persist::ProgressPair,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SceneSaveError {
    #[error("the scene request exceeds its bounds or has an invalid key")]
    InvalidRequest,
    #[error("the scene's story precondition changed")]
    StaleStory,
    #[error("the scene requires existing character progress")]
    MissingCharacter,
    #[error("the scene's gil precondition changed")]
    StaleGil,
    #[error("the scene's durable checkpoint or participant resources changed")]
    StaleState,
    #[error("the battle consumed an item absent from its durable starting inventory")]
    InsufficientItems,
    #[error("the scene battle settlement requires battle rules")]
    MissingRules,
    #[error("the saver is not accepting scene requests")]
    Stopped,
    #[error("the scene save worker did not complete; reopen the store before retrying")]
    WorkerFailed,
    #[error(transparent)]
    Store(#[from] PersistError),
}

impl SceneSaveJob {
    /// Stable selected-result identity. Length framing keeps arbitrary game blobs unambiguous;
    /// timestamps are excluded; the original resource precondition and result remain stable
    /// across a retry even when later rewards have changed the stored total. The receipt itself binds the prior story and scene.
    fn selected_result(&self) -> Result<Vec<u8>, SceneSaveError> {
        const HEADER: &[u8] = b"fmv-scene-gil-v2\0";
        let overhead = HEADER.len() + 12;
        if self.scene_key.is_empty() || self.scene_key.len() > 256 || self.scene_key.chars().any(char::is_control)
            || self.expected_story.len() > fmv_protocol::MAX_STORY_PROGRESS_BYTES
            || self.next_story.len() > fmv_protocol::MAX_STORY_PROGRESS_BYTES {
            return Err(SceneSaveError::InvalidRequest);
        }
        let mut result = Vec::with_capacity(overhead + self.next_story.len());
        result.extend_from_slice(HEADER);
        result.extend_from_slice(&(self.next_story.len() as u32).to_le_bytes());
        result.extend_from_slice(&self.next_story);
        result.extend_from_slice(&self.expected_gil.to_le_bytes());
        result.extend_from_slice(&self.next_gil.to_le_bytes());
        Ok(result)
    }
}

/// What the saver's queue carries.
pub(crate) enum Msg {
    Save(SaveJob),
    Story(StoryJob),
    /// Repeatable exact story transition, with no permanent scene receipt.
    StoryCas { job: StoryCasJob, done: oneshot::Sender<Result<SceneSaved, SceneSaveError>> },
    /// Ordinary story battle terminal resources and owner progression, acknowledged together.
    StoryBattle { job: StoryBattleJob, done: oneshot::Sender<Result<fmv_persist::SceneBatchCommitted, SceneSaveError>> },
    /// Coherent ordinary battle baseline with room for its terminal receipt, before combat.
    StoryBattleSnapshot { accounts: Vec<AccountId>, done: oneshot::Sender<Result<Vec<fmv_persist::SceneState>, SceneSaveError>> },
    /// Ordered with rewards and ordinary saves, acknowledged only after a durable transaction.
    Scene { job: SceneSaveJob, done: oneshot::Sender<Result<SceneSaved, SceneSaveError>> },
    /// Every participant's checkpoint, costs and earned resources commit in one durable operation.
    SceneBatch { job: SceneBatchJob, done: oneshot::Sender<Result<fmv_persist::SceneBatchCommitted, SceneSaveError>> },
    /// A coherent baseline after all earlier queued rewards/story writes, without holding the world lock.
    SceneSnapshot { accounts: Vec<AccountId>, done: oneshot::Sender<Result<Vec<fmv_persist::SceneState>, SceneSaveError>> },
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

    /// Queue the story progression `account` has now. `false` if the saver has stopped.
    pub(crate) fn save_story(&self, account: AccountId, progress: Vec<u8>) -> bool {
        self.tx.send(Msg::Story(StoryJob { account, progress })).is_ok()
    }

    pub(crate) fn save_story_cas(&self, job: StoryCasJob) -> Result<oneshot::Receiver<Result<SceneSaved, SceneSaveError>>, SceneSaveError> {
        job.validate()?;
        let (done, finished) = oneshot::channel();
        self.tx.send(Msg::StoryCas { job, done }).map_err(|_| SceneSaveError::Stopped)?;
        Ok(finished)
    }

    pub(crate) fn save_story_battle(&self, job: StoryBattleJob) -> Result<oneshot::Receiver<Result<fmv_persist::SceneBatchCommitted, SceneSaveError>>, SceneSaveError> {
        job.validate()?;
        let (done, finished) = oneshot::channel();
        self.tx.send(Msg::StoryBattle { job, done }).map_err(|_| SceneSaveError::Stopped)?;
        Ok(finished)
    }

    pub(crate) fn snapshot_story_battle(&self, accounts: Vec<AccountId>) -> Result<oneshot::Receiver<Result<Vec<fmv_persist::SceneState>, SceneSaveError>>, SceneSaveError> {
        continuation::validate_accounts(&accounts)?;
        let (done, finished) = oneshot::channel();
        self.tx.send(Msg::StoryBattleSnapshot { accounts, done }).map_err(|_| SceneSaveError::Stopped)?;
        Ok(finished)
    }

    /// Queue a complete scene decision. Dropping the receiver never cancels a queued commit.
    /// An enqueue error is returned immediately; a closed acknowledgement is an uncertain result.
    pub(crate) fn save_scene(&self, job: SceneSaveJob) -> Result<oneshot::Receiver<Result<SceneSaved, SceneSaveError>>, SceneSaveError> {
        job.selected_result()?;
        let (done, finished) = oneshot::channel();
        self.tx.send(Msg::Scene { job, done }).map_err(|_| SceneSaveError::Stopped)?;
        Ok(finished)
    }

    /// Queue a checkpoint or a shared battle settlement. A dropped receiver does not cancel it.
    pub(crate) fn save_scene_batch(&self, job: SceneBatchJob) -> Result<oneshot::Receiver<Result<fmv_persist::SceneBatchCommitted, SceneSaveError>>, SceneSaveError> {
        job.validate()?;
        let (done, finished) = oneshot::channel();
        self.tx.send(Msg::SceneBatch { job, done }).map_err(|_| SceneSaveError::Stopped)?;
        Ok(finished)
    }

    /// Read an exact baseline ordered after prior writes. The world must reserve these accounts
    /// before requesting it and retain that reservation through its following acknowledged commit.
    pub(crate) fn snapshot_scene_batch(&self, accounts: Vec<AccountId>) -> Result<oneshot::Receiver<Result<Vec<fmv_persist::SceneState>, SceneSaveError>>, SceneSaveError> {
        continuation::validate_accounts(&accounts)?;
        let (done, finished) = oneshot::channel();
        self.tx.send(Msg::SceneSnapshot { accounts, done }).map_err(|_| SceneSaveError::Stopped)?;
        Ok(finished)
    }
}

/// The saver task and the way to stop it.
pub(crate) struct Saver {
    handle: SaverHandle,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Saver {
    /// Start draining a new queue into `store` for `game`. `rules` are the battle rules of a server
    /// that runs battles (payments need them; a server without any never queues one), `adapter` is
    /// what merges a story save into the stored progression. Needs a running tokio runtime.
    pub(crate) fn spawn(store: Arc<Store>, game: GameId, rules: Option<Arc<dyn BattleRules>>, adapter: Arc<dyn GameAdapter>) -> Self {
        let (handle, rx) = SaverHandle::channel();
        let task = tokio::spawn(run(rx, store, game, rules, adapter));
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
    let record = store.update_character_progress(job.account, game, |mut current| {
        // Accepting an item command changes inventory, not earned resources. In particular,
        // an authored full-u32 gil value must not be clamped merely because a potion was used.
        // Missing records keep the historical default initialization in the general path below.
        if let (SavedChange::UsedItem(item), Some(record)) = (&job.change, current.as_mut()) {
            let mut inventory = Inventory::resolve(rules, record.inventory.as_deref());
            inventory.apply(&Change::Used(*item), rules.max_item_stack());
            items = inventory.total();
            record.inventory = Some(inventory.entries().to_vec());
            record.updated_at = now;
            return record.clone();
        }
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

/// Merge `job.progress` into the stored progression of `job.account` with the adapter's own rule, as
/// one atomic read-modify-write, and return the progression that is stored now. The blob is only
/// rewritten (and `updated_at` moved) when the merge changes it. Blocking: run it off the executor.
pub(crate) fn persist_story(
    store: &Store,
    game: GameId,
    adapter: &dyn GameAdapter,
    job: &StoryJob,
    now: u64,
) -> Result<fmv_persist::Progression, PersistError> {
    store.update_progression(job.account, game, |mut current| {
        let merged = adapter.merge_progression(&current.blob, &job.progress);
        if merged != current.blob {
            current.blob = merged;
            current.updated_at = now;
        }
        current
    })
}

/// Save a scene without consulting the world's stale resource mirror. No failed transaction is
/// retried here: after an uncertain write the store requires recovery, and a fresh request must
/// check its receipt before calculating a debit again.
pub(crate) fn persist_scene(store: &Store, game: GameId, job: &SceneSaveJob, now: u64) -> Result<SceneSaved, SceneSaveError> {
    let selected = job.selected_result()?;
    let expected_story = fmv_persist::Progression::new(job.account, game, job.expected_story.clone(), 0);
    if store.scene_progress_applied(&job.scene_key, &expected_story, &selected)? {
        return Ok(SceneSaved { commit: fmv_persist::SceneCommit::AlreadyApplied, pair: store.get_progress_pair(job.account, game)? });
    }
    let before = store.get_progress_pair(job.account, game)?;
    if before.story.blob != job.expected_story { return Err(SceneSaveError::StaleStory); }
    let mut next = before.clone();
    let character = next.character.as_mut().ok_or(SceneSaveError::MissingCharacter)?;
    if character.gil != job.expected_gil { return Err(SceneSaveError::StaleGil); }
    character.gil = job.next_gil;
    character.updated_at = now;
    next.story.blob = job.next_story.clone();
    next.story.updated_at = now;
    let commit = store.commit_scene_progress_with_result(&job.scene_key, &selected, &before, &next)?;
    // Read the stamped schema and actual current pair, including any independent later writer.
    Ok(SceneSaved { commit, pair: store.get_progress_pair(job.account, game)? })
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Msg>,
    store: Arc<Store>,
    game: GameId,
    rules: Option<Arc<dyn BattleRules>>,
    adapter: Arc<dyn GameAdapter>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Save(job) => match &rules {
                Some(rules) => save_one(&store, game, rules, job).await,
                // A server without battle rules never queues a payment; if one arrives it cannot be applied.
                None => error!(?job, "a battle payment reached a saver that has no battle rules; it is lost"),
            },
            Msg::Story(job) => save_story(&store, game, &adapter, job).await,
            Msg::StoryCas { job, done } => {
                let account = job.account;
                let (store, adapter) = (Arc::clone(&store), Arc::clone(&adapter));
                let result = tokio::task::spawn_blocking(move || persist_story_cas(&store, game, adapter.as_ref(), &job, unix_now()))
                    .await.unwrap_or(Err(SceneSaveError::WorkerFailed));
                if result.is_ok() { info!(account = account.0, "exact story transaction acknowledged"); }
                else { error!(account = account.0, "exact story transaction not acknowledged; progress must remain held"); }
                let _ = done.send(result);
            }
            Msg::StoryBattle { job, done } => {
                let members = job.members.len();
                let (store, adapter, rules) = (Arc::clone(&store), Arc::clone(&adapter), rules.clone());
                let result = tokio::task::spawn_blocking(move || persist_story_battle(&store, game, rules.as_deref(), adapter.as_ref(), &job, unix_now()))
                    .await.unwrap_or(Err(SceneSaveError::WorkerFailed));
                if result.is_ok() { info!(members, "story battle outcome acknowledged"); }
                else { error!(members, "story battle outcome not acknowledged; participants must remain held"); }
                let _ = done.send(result);
            }
            Msg::StoryBattleSnapshot { accounts, done } => {
                let store = Arc::clone(&store);
                let result = tokio::task::spawn_blocking(move || store.get_battle_outcome_states(&accounts, game).map_err(SceneSaveError::from))
                    .await.unwrap_or(Err(SceneSaveError::WorkerFailed));
                let _ = done.send(result);
            }
            Msg::Scene { job, done } => {
                let account = job.account;
                let store = Arc::clone(&store);
                let result = tokio::task::spawn_blocking(move || persist_scene(&store, game, &job, unix_now()))
                    .await.unwrap_or(Err(SceneSaveError::WorkerFailed));
                // No story bytes, selected choices, or request Debug representation enter logs.
                if result.is_ok() { info!(account = account.0, "scene transaction acknowledged"); }
                else { error!(account = account.0, "scene transaction not acknowledged; player progress must remain held"); }
                let _ = done.send(result);
            }
            Msg::SceneBatch { job, done } => {
                let members = job.members.len();
                let store = Arc::clone(&store);
                let rules = rules.clone();
                let result = tokio::task::spawn_blocking(move || persist_scene_batch(&store, game, rules.as_deref(), &job, unix_now()))
                    .await.unwrap_or(Err(SceneSaveError::WorkerFailed));
                if result.is_ok() { info!(members, "scene batch transaction acknowledged"); }
                else { error!(members, "scene batch transaction not acknowledged; participants must remain held"); }
                let _ = done.send(result);
            }
            Msg::SceneSnapshot { accounts, done } => {
                let store = Arc::clone(&store);
                let result = tokio::task::spawn_blocking(move || store.get_scene_states(&accounts, game).map_err(SceneSaveError::from))
                    .await.unwrap_or(Err(SceneSaveError::WorkerFailed));
                let _ = done.send(result);
            }
            Msg::Stop(done) => {
                let _ = done.send(());
                break;
            }
        }
    }
}

async fn save_story(store: &Arc<Store>, game: GameId, adapter: &Arc<dyn GameAdapter>, job: StoryJob) {
    let mut waits = RETRY_WAITS.into_iter();
    let mut attempt = 1u32;
    loop {
        let (store, adapter, attempt_job) = (Arc::clone(store), Arc::clone(adapter), job.clone());
        let result = tokio::task::spawn_blocking(move || persist_story(&store, game, adapter.as_ref(), &attempt_job, unix_now())).await;
        let failure = match result {
            Ok(Ok(saved)) => {
                // `key=value` layout, new keys at the end: `scripts/phase-smoke.sh` counts these lines.
                info!("story saved account={} bytes={} progress={}", job.account.0, saved.blob.len(), hex_of(&saved.blob));
                return;
            }
            Ok(Err(failure)) => failure,
            Err(e) => {
                error!(error = %e, ?job, "the story write task failed; the story step is lost");
                return;
            }
        };
        match waits.next() {
            Some(wait) if is_transient(&failure) => {
                warn!(account = job.account.0, attempt, error = %failure, "could not save story progress; trying again");
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            _ => {
                error!(
                    "story progress NOT SAVED (the player has it, the store does not): account={} progress={} error={failure}",
                    job.account.0,
                    hex_of(&job.progress)
                );
                return;
            }
        }
    }
}

/// Lower-case hex of `bytes`, at most 32 of them (a story blob is a few bytes).
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().take(32).map(|b| format!("{b:02x}")).collect()
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
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, Some(rules()), adapter());
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
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, Some(rules()), adapter());
        let handle = saver.handle();
        assert!(handle.save(job(AccountId(404), 0, 9, 9, 9))); // no such account: refused, not retried
        assert!(handle.save(job(account, 0, 5, 6, 7)));
        saver.shutdown(Duration::from_secs(20)).await;
        let p = stored(&store, account).unwrap();
        assert_eq!((p.exp, p.gil, p.ap), (45, 6, 7));
        let ghost = store.get_character_progress(AccountId(404), GameId::Ff7);
        assert!(matches!(ghost, Err(PersistError::UnknownAccount(_))), "nothing was made for the ghost: {ghost:?}");
    }

    // ---- story saves ----

    #[test]
    fn an_item_only_save_preserves_every_unrelated_stored_resource_without_normalization() {
        let (_dir, store, account) = store();
        let mut before = fmv_persist::CharacterProgress::new(account, GameId::Ff7, 9, u8::MAX, NOW);
        before.exp = u32::MAX;
        before.ap = u32::MAX - 1;
        before.gil = u32::MAX;
        before.limit = u16::MAX;
        before.inventory = Some(vec![e(0, 3)]);
        store.put_character_progress(&before).unwrap();
        let before = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        persist(&store, GameId::Ff7, rules().as_ref(), &SaveJob::used_item(account, 0, 0), NOW + 1).unwrap();
        let mut expected = before;
        expected.inventory = Some(vec![e(0, 2)]);
        expected.updated_at = NOW + 1;
        assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap(), Some(expected));
    }

    /// An adapter that merges progressions like FF7's does (the further moment wins, an unreadable
    /// record stays); nothing else of it is used by the saver.
    struct Ff7Merge;
    impl GameAdapter for Ff7Merge {
        fn game(&self) -> GameId {
            GameId::Ff7
        }
        fn list_areas(&self) -> Vec<fmv_adapter_api::AreaInfo> {
            Vec::new()
        }
        fn load_area(&self, key: &str) -> Result<Box<dyn fmv_adapter_api::AreaRuntime>, fmv_adapter_api::AdapterError> {
            Err(fmv_adapter_api::AdapterError::AreaNotFound(key.to_string()))
        }
        fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
            fmv_ff7::story::merge_progression(stored, advanced)
        }
    }

    fn adapter() -> Arc<dyn GameAdapter> {
        Arc::new(Ff7Merge)
    }

    fn moment(m: u16) -> Vec<u8> {
        fmv_ff7::story::StoryState::at(m).encode()
    }

    fn stored_story(store: &Store, account: AccountId) -> Vec<u8> {
        store.get_progression(account, GameId::Ff7).unwrap().blob
    }

    fn scene_job(account: AccountId, debit: u32) -> SceneSaveJob {
        SceneSaveJob { account, scene_key: "ff7/md8_2/flower".into(), expected_story: moment(36), next_story: moment(39), expected_gil: 1, next_gil: 1 - debit }
    }

    fn scene_store(gil: u32) -> (tempfile::TempDir, Arc<Store>, AccountId) {
        let (dir, store, account) = store();
        persist(&store, GameId::Ff7, rules().as_ref(), &job(account, 0, 12, gil, 2), NOW).unwrap();
        store.put_progression(&fmv_persist::Progression::new(account, GameId::Ff7, moment(36), NOW)).unwrap();
        let mut character = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        character.limit = 137;
        character.inventory = Some(vec![e(2, 7), e(9, 1)]);
        store.put_character_progress(&character).unwrap();
        (dir, store, account)
    }

    #[tokio::test]
    async fn scene_save_acknowledges_after_earlier_rewards_and_story_writes_and_preserves_other_progress() {
        let (_dir, store, account) = scene_store(0);
        let before = store.get_progress_pair(account, GameId::Ff7).unwrap().character.unwrap();
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, Some(rules()), adapter());
        let handle = saver.handle();
        assert!(handle.save(job(account, 0, 0, 1, 0)), "the gil is still queued, not in the caller's snapshot");
        assert!(handle.save_story(account, moment(37)));
        let request = SceneSaveJob { expected_story: moment(37), ..scene_job(account, 1) };
        let saved = handle.save_scene(request).unwrap().await.unwrap().unwrap();
        assert_eq!(saved.commit, fmv_persist::SceneCommit::Applied);
        assert_eq!(saved.pair.story.blob, moment(39));
        let mut want = before;
        want.schema_version = 2;
        want.updated_at = saved.pair.character.as_ref().unwrap().updated_at;
        assert_eq!(saved.pair.character, Some(want), "only the earned gil was spent; inventory, exp, AP, level and limit survive");
        assert_eq!(store.get_progress_pair(account, GameId::Ff7).unwrap(), saved.pair, "acknowledgement describes stored records");
        saver.shutdown(Duration::from_secs(20)).await;
    }

    #[test]
    fn scene_save_refuses_stale_gil_or_story_missing_character_and_oversized_requests_without_writes() {
        let (_dir, store, account) = scene_store(0);
        let before = store.get_progress_pair(account, GameId::Ff7).unwrap();
        assert!(matches!(persist_scene(&store, GameId::Ff7, &scene_job(account, 1), NOW + 1), Err(SceneSaveError::StaleGil)));
        assert!(matches!(persist_scene(&store, GameId::Ff7, &SceneSaveJob { expected_story: moment(35), ..scene_job(account, 0) }, NOW + 1), Err(SceneSaveError::StaleStory)));
        assert!(matches!(persist_scene(&store, GameId::Ff7, &SceneSaveJob { next_story: vec![0; fmv_protocol::MAX_STORY_PROGRESS_BYTES + 1], ..scene_job(account, 0) }, NOW + 1), Err(SceneSaveError::InvalidRequest)));
        assert!(matches!(persist_scene(&store, GameId::Ff7, &SceneSaveJob { scene_key: "bad\nkey".into(), ..scene_job(account, 0) }, NOW + 1), Err(SceneSaveError::InvalidRequest)));
        assert_eq!(store.get_progress_pair(account, GameId::Ff7).unwrap(), before);
        let (fresh, _) = store.create_account("Fresh", NOW).unwrap();
        let request = SceneSaveJob { expected_story: Vec::new(), ..scene_job(fresh, 0) };
        assert!(matches!(persist_scene(&store, GameId::Ff7, &request, NOW + 1), Err(SceneSaveError::MissingCharacter)));
        assert!(store.get_character_progress(fresh, GameId::Ff7).unwrap().is_none());
        assert!(store.get_progression(fresh, GameId::Ff7).unwrap().blob.is_empty());
    }

    #[test]
    fn scene_save_does_not_infer_a_receipt_from_matching_current_story() {
        let (_dir, store, account) = scene_store(1);
        // Another path wrote the same story, without this scene's once-only receipt.
        store.put_progression(&fmv_persist::Progression::new(account, GameId::Ff7, moment(39), NOW + 1)).unwrap();
        let before = store.get_progress_pair(account, GameId::Ff7).unwrap();
        assert!(matches!(persist_scene(&store, GameId::Ff7, &scene_job(account, 1), NOW + 2), Err(SceneSaveError::StaleStory)));
        assert_eq!(store.get_progress_pair(account, GameId::Ff7).unwrap(), before);
    }

    #[test]
    fn scene_save_nonpurchase_advances_the_story_without_changing_resources() {
        let (_dir, store, account) = scene_store(23);
        let mut character = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
        let request = SceneSaveJob { expected_gil: 23, next_gil: 23, ..scene_job(account, 0) };
        let saved = persist_scene(&store, GameId::Ff7, &request, NOW + 1).unwrap();
        character.schema_version = 2;
        character.updated_at = NOW + 1;
        assert_eq!(saved.pair.character, Some(character));
        assert_eq!(saved.pair.story.blob, moment(39));
        assert_eq!(persist_scene(&store, GameId::Ff7, &request, NOW + 2).unwrap().commit, fmv_persist::SceneCommit::AlreadyApplied);
    }

    #[test]
    fn scene_save_receipt_survives_rewards_and_reopen_without_a_second_debit() {
        let (dir, store, account) = scene_store(1);
        let request = scene_job(account, 1);
        let applied = persist_scene(&store, GameId::Ff7, &request, NOW + 1).unwrap();
        assert_eq!(applied.pair.character.unwrap().gil, 0, "exact funds are valid");
        persist(&store, GameId::Ff7, rules().as_ref(), &job(account, 0, 12, 20, 3), NOW + 2).unwrap();
        let later = store.get_progress_pair(account, GameId::Ff7).unwrap();
        drop(store);
        let reopened = Store::open(dir.path().join("state")).unwrap();
        let repeated = persist_scene(&reopened, GameId::Ff7, &request, NOW + 3).unwrap();
        assert_eq!(repeated.commit, fmv_persist::SceneCommit::AlreadyApplied);
        assert_eq!(repeated.pair, later, "a duplicate returns current progress including later rewards");
        let different = SceneSaveJob { next_gil: 1, ..request };
        assert!(matches!(persist_scene(&reopened, GameId::Ff7, &different, NOW + 3), Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
        assert_eq!(reopened.get_progress_pair(account, GameId::Ff7).unwrap(), later);
    }

    #[tokio::test]
    async fn scene_save_continues_after_refusal_and_dropping_the_acknowledgement_does_not_cancel_commit() {
        let (_dir, store, account) = scene_store(0);
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, Some(rules()), adapter());
        let handle = saver.handle();
        assert!(matches!(handle.save_scene(scene_job(account, 1)).unwrap().await.unwrap(), Err(SceneSaveError::StaleGil)));
        assert!(handle.save(job(account, 0, 0, 1, 0)));
        let finished = handle.save_scene(scene_job(account, 1)).unwrap();
        drop(finished); // disconnect after enqueue
        saver.shutdown(Duration::from_secs(20)).await;
        assert_eq!(stored_story(&store, account), moment(39));
        assert_eq!(stored(&store, account).unwrap().gil, 0);
        assert_eq!(persist_scene(&store, GameId::Ff7, &scene_job(account, 1), NOW + 9).unwrap().commit, fmv_persist::SceneCommit::AlreadyApplied);
        assert!(matches!(handle.save_scene(scene_job(account, 1)), Err(SceneSaveError::Stopped)));
    }

    #[test]
    fn scene_save_keeps_extended_story_zeroes_clears_and_numeric_values_exactly() {
        let (_dir, store, account) = scene_store(1);
        let mut before = fmv_ff7::story::StoryState::at(36).into_extended().unwrap();
        before.write_masked(1, 4, 30, 255).unwrap();
        before.write_masked(3, 66, 1, 1).unwrap();
        let mut after = before.clone();
        after.write_masked(1, 4, 35, 255).unwrap();
        after.write_masked(3, 66, 0, 1).unwrap();
        after.moment = 39;
        let old = before.encode();
        let next = after.encode();
        assert_eq!(fmv_ff7::story::compare_exchange_progression(&old, &old, &next), Some(next.clone()), "the host validates its interpreted result before submission");
        store.put_progression(&fmv_persist::Progression::new(account, GameId::Ff7, old.clone(), NOW)).unwrap();
        let request = SceneSaveJob { expected_story: old, next_story: next.clone(), ..scene_job(account, 1) };
        let saved = persist_scene(&store, GameId::Ff7, &request, NOW + 1).unwrap();
        assert_eq!(saved.pair.story.blob, next);
        assert_eq!(fmv_ff7::story::StoryState::decode(&saved.pair.story.blob), Some(after));
    }

    #[tokio::test]
    async fn story_saves_merge_forward_whatever_order_they_arrive_in_and_keep_the_character_record_alone() {
        let (_dir, store, account) = store();
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, Some(rules()), adapter());
        let handle = saver.handle();
        // A payment and story saves share one queue and do not touch each other's record.
        assert!(handle.save(job(account, 0, 12, 20, 2)));
        assert!(handle.save_story(account, moment(7)));
        assert!(handle.save_story(account, moment(6)), "a late, older save...");
        assert!(handle.save_story(account, Vec::new()), "...and a new account's blob...");
        saver.shutdown(Duration::from_secs(20)).await;
        assert_eq!(stored_story(&store, account), moment(7), "...cannot take the story back");
        let p = stored(&store, account).expect("the payment was saved too");
        assert_eq!((p.exp, p.gil, p.ap), (52, 20, 2));
        // Nothing but the story blob changed in the story record, and it is a plain v7-shaped one.
        let record = store.get_progression(account, GameId::Ff7).unwrap();
        assert_eq!((record.schema_version, record.account_id, record.game), (fmv_persist::SCHEMA_VERSION, account, GameId::Ff7));
    }

    #[tokio::test]
    async fn a_story_record_the_adapter_cannot_read_is_never_overwritten_by_a_save() {
        let (_dir, store, account) = store();
        store.put_progression(&fmv_persist::Progression::new(account, GameId::Ff7, vec![7, 7], NOW)).unwrap();
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, None, adapter());
        assert!(saver.handle().save_story(account, moment(9)));
        saver.shutdown(Duration::from_secs(20)).await;
        assert_eq!(stored_story(&store, account), vec![7, 7], "the opaque blob is still there");
    }

    #[tokio::test]
    async fn a_story_save_that_cannot_be_written_does_not_hold_up_the_ones_behind_it_and_a_server_without_battles_saves_stories() {
        let (_dir, store, account) = store();
        // No battle rules at all (a server without battle data), and a save for an account that does not exist.
        let saver = Saver::spawn(Arc::clone(&store), GameId::Ff7, None, adapter());
        let handle = saver.handle();
        assert!(handle.save_story(AccountId(404), moment(6)));
        assert!(handle.save_story(account, moment(6)));
        saver.shutdown(Duration::from_secs(20)).await;
        assert_eq!(stored_story(&store, account), moment(6));
        assert!(matches!(store.get_progression(AccountId(404), GameId::Ff7), Err(PersistError::UnknownAccount(_))));
    }

    #[test]
    fn persisting_a_story_rewrites_the_record_only_when_the_merge_changes_it() {
        let (_dir, store, account) = store();
        let rules = adapter();
        let saved = persist_story(&store, GameId::Ff7, rules.as_ref(), &StoryJob { account, progress: moment(6) }, NOW).unwrap();
        assert_eq!((saved.blob.clone(), saved.updated_at), (moment(6), NOW));
        // An equal or older story leaves the blob, and the time of the write that made it, as they are.
        let again = persist_story(&store, GameId::Ff7, rules.as_ref(), &StoryJob { account, progress: moment(6) }, NOW + 50).unwrap();
        assert_eq!((again.blob, again.updated_at), (moment(6), NOW));
        let older = persist_story(&store, GameId::Ff7, rules.as_ref(), &StoryJob { account, progress: moment(2) }, NOW + 60).unwrap();
        assert_eq!((older.blob, older.updated_at), (moment(6), NOW));
        let newer = persist_story(&store, GameId::Ff7, rules.as_ref(), &StoryJob { account, progress: moment(8) }, NOW + 70).unwrap();
        assert_eq!((newer.blob, newer.updated_at), (moment(8), NOW + 70));
    }

    /// **The migration of the story blob (version 1 to version 2).** A progression record in exactly the shape a v9 build
    /// wrote (its five fields, the blob `[1, lo, hi]`) loads as it always did (the moment, no variable); the elevator's ride
    /// adds the lift bit to it, as a version 2 blob in the very same record shape; a late save from before the ride (an older
    /// moment, version 1) changes nothing, and what is stored reads back as the state the ride made.
    #[test]
    fn a_v9_story_record_loads_as_before_gains_the_lift_bit_and_a_late_older_save_cannot_take_it_back() {
        use fmv_ff7::story::StoryState;
        use fmv_ff7::script::dialogue::StoryVars;
        let (dir, store, account) = store();
        let adapter = adapter();
        let record = dir.path().join("state").join("progression").join(account.0.to_string()).join("Ff7.json");
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        let keys = |path: &std::path::Path| -> (Vec<String>, String) {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let mut names: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            names.sort();
            (names, v["blob"].as_str().unwrap().to_string())
        };

        // What a v9 build wrote for an account at moment 6, by its field names.
        let v9 = serde_json::json!({ "schema_version": fmv_persist::SCHEMA_VERSION, "account_id": account.0, "game": "Ff7", "blob": "010600", "updated_at": 7 });
        std::fs::write(&record, serde_json::to_vec_pretty(&v9).unwrap()).unwrap();
        let loaded = store.get_progression(account, GameId::Ff7).unwrap();
        assert_eq!((loaded.blob.clone(), loaded.updated_at), (vec![1, 6, 0], 7), "the record is read exactly as before");
        let story = StoryState::decode(&loaded.blob).expect("a version 1 blob is read");
        assert_eq!((story.moment, story.vars().count()), (6, 0), "no progress reset: the moment is what was stored, and no variable");
        assert_eq!(story.byte(1, 225), 0, "the dialogue hook reads the lift as up");
        let shape = keys(&record).0;
        assert_eq!(shape, ["account_id", "blob", "game", "schema_version", "updated_at"]);

        // The ride (moment 6 on to 12, the lift bit on) is saved over it.
        let mut rode = StoryState::at(12);
        assert!(rode.set_bit(1, 225, 0, true));
        let saved = persist_story(&store, GameId::Ff7, adapter.as_ref(), &StoryJob { account, progress: rode.encode() }, NOW + 10).unwrap();
        assert_eq!((saved.blob.clone(), saved.updated_at), (vec![2, 12, 0, 1, 1, 225, 1], NOW + 10));
        assert_eq!(keys(&record), (shape.clone(), "020c000101e101".to_string()), "the same record shape, a longer blob");
        assert_eq!(StoryState::decode(&store.get_progression(account, GameId::Ff7).unwrap().blob), Some(rode.clone()), "it reads back as the state the ride made");

        // A late save from before the ride, in either version, takes nothing back and does not even rewrite the record.
        for late in [StoryState::at(7).encode(), StoryState::at(12).encode(), Vec::new()] {
            let again = persist_story(&store, GameId::Ff7, adapter.as_ref(), &StoryJob { account, progress: late }, NOW + 99).unwrap();
            assert_eq!((again.blob, again.updated_at), (vec![2, 12, 0, 1, 1, 225, 1], NOW + 10));
        }
        // ...while a further moment from a client that never heard of the variable keeps the bit it was never told of.
        let further = persist_story(&store, GameId::Ff7, adapter.as_ref(), &StoryJob { account, progress: StoryState::at(13).encode() }, NOW + 20).unwrap();
        let mut expected = StoryState::at(13);
        expected.set_bit(1, 225, 0, true);
        assert_eq!((further.blob, further.updated_at), (expected.encode(), NOW + 20));
        // A v9-shaped record of a new account (an empty blob) is a new account to version 2 as well.
        let (_dir2, store2, fresh) = self::store();
        let created = store2.get_progression(fresh, GameId::Ff7).unwrap();
        assert!(created.blob.is_empty() && StoryState::decode(&created.blob) == Some(StoryState::new_game()));
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
