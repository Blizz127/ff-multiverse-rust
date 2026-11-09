//! The [`Store`] side of the player-character record (the record itself is in `character.rs`).
//!
//! Three operations, none of which ever edits a file an older build wrote:
//!
//! * [`Store::get_player_character`]: a lock-free read (the file is replaced atomically, so it is the whole
//!   old record or the whole new one);
//! * [`Store::ensure_player_character`]: the migration. An account that predates the record (every account
//!   of a v6 or v7 server) gets one the first time it logs in, **added next to** its avatar profile and its
//!   progress records, which are not touched, so a rolled-back build finds them byte for byte as it left
//!   them. Reads lock-free first and takes the writer lock only to add the file (once per account and game);
//! * [`Store::linked_character_progress`]: follows the pointer from the record to the character-progress record.
//!
//! Creation together with the account is [`Store::register_with_character`], which is in `store.rs` because it
//! shares the registration's crash-consistency argument.

use std::collections::BTreeMap;

use fmv_core::{AccountId, GameId};

use super::Store;
use crate::character::{
    CharacterOrigin, GameCharacter, Identity, LegacyView, LinkedProgress, NewPlayerCharacter, PlayerCharacter, ProgressRef,
    MAX_PLAYER_CHARACTER_BYTES,
};
use crate::error::{PersistError, Result};
use crate::fsio;
use crate::model::{game_key, SCHEMA_VERSION};

/// The tint of an identity migrated from an account that has no avatar profile at all (made by other tooling).
const NEUTRAL_TINT: [u8; 3] = [160, 160, 160];

/// A new character's record for `account`, as a registration creates it.
pub(super) fn new_record(account: AccountId, name: &str, tint: [u8; 3], new: &NewPlayerCharacter, now: u64) -> PlayerCharacter {
    PlayerCharacter {
        schema_version: SCHEMA_VERSION,
        account_id: account,
        identity: Identity { name: name.to_owned(), tint, body: new.body.clone() },
        games: new.games.clone(),
        origin: CharacterOrigin::Created,
        created_at: now,
        updated_at: now,
    }
}

impl Store {
    /// The account's [`PlayerCharacter`], or `None` if it has none yet (an account that predates the record: see
    /// [`Store::ensure_player_character`]). Fails with [`PersistError::UnknownAccount`] for an account that does
    /// not exist and with [`PersistError::Corrupt`] / [`PersistError::FutureSchema`] / [`PersistError::TooLarge`]
    /// for a file that is damaged, written by a newer build or oversized: such a file is never replaced by a
    /// default behind the caller's back.
    ///
    /// Takes no writer lock: a login never waits for a write to read it.
    pub fn get_player_character(&self, account: AccountId) -> Result<Option<PlayerCharacter>> {
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        self.read_player_character(account)
    }

    fn read_player_character(&self, account: AccountId) -> Result<Option<PlayerCharacter>> {
        let path = self.character_record_path(account.0);
        let Some(record) = fsio::read_json::<PlayerCharacter>(&path, MAX_PLAYER_CHARACTER_BYTES)? else {
            return Ok(None);
        };
        if record.account_id != account {
            return Err(PersistError::corrupt(&path, format!("record is for account {:?}, not the requested account {account:?}", record.account_id)));
        }
        record.validate().map_err(|e| PersistError::corrupt(&path, e.to_string()))?;
        Ok(Some(record))
    }

    /// The account's [`PlayerCharacter`] with an entry for `game`, made now if it is missing: the **migration** of
    /// an account that predates the record (every account of a v6 or v7 server), and the first entry of a
    /// character into a game it has not played. Returns the record and whether this call added something.
    ///
    /// What is added is built by `legacy` from what the account already has for `game` ([`LegacyView`]: its
    /// avatar profile and its character-progress record) and **nothing the account already has is written, moved
    /// or re-keyed**: the avatar profile, the character-progress record and the story blob stay byte for byte as
    /// they are. A brand-new record has the account's name and colour as its identity (no body: none was ever
    /// chosen) and the origin [`CharacterOrigin::Migrated`]; an existing record (a character that has played another
    /// game) only gains the entry.
    ///
    /// `legacy` runs while the store's writer lock is held: it must be quick and must not call back into this
    /// store. It runs at most once, and not at all when the record already has an entry for `game`; the existing
    /// record is then returned as it is. The lock is taken only to add the file, after a lock-free read found
    /// nothing to return (the documented exception to "a login never waits for a write", once per account and game).
    /// What `legacy` returns must pass the record's checks ([`PlayerCharacter`]); otherwise nothing is written.
    pub fn ensure_player_character<F>(&self, account: AccountId, game: GameId, now: u64, legacy: F) -> Result<(PlayerCharacter, bool)>
    where
        F: FnOnce(&LegacyView<'_>) -> GameCharacter,
    {
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        let key = game_key(game);
        if let Some(record) = self.read_player_character(account)? {
            if record.games.contains_key(&key) {
                return Ok((record, false));
            }
        }

        // Slow path: add the missing record or entry. Another login of the same account may have done it
        // while this one waited for the lock, so look again under it.
        let _writers = self.writer();
        let existing = self.read_player_character(account)?;
        if let Some(record) = &existing {
            if record.games.contains_key(&key) {
                return Ok((record.clone(), false));
            }
        }
        let name = self.load_account(account.0)?.display_name;
        let avatar = self.avatar_for_account(account)?;
        let progress = self.read_character_progress(account, game)?;
        let entry = legacy(&LegacyView { name: &name, avatar: avatar.as_ref(), progress: progress.as_ref() });

        let mut record = existing.unwrap_or_else(|| PlayerCharacter {
            schema_version: SCHEMA_VERSION,
            account_id: account,
            identity: Identity {
                name: avatar.as_ref().map_or_else(|| name.clone(), |a| a.display_name.clone()),
                tint: avatar.as_ref().map_or(NEUTRAL_TINT, |a| a.tint),
                body: String::new(),
            },
            games: BTreeMap::new(),
            origin: CharacterOrigin::Migrated,
            created_at: now,
            updated_at: now,
        });
        record.games.insert(key, entry);
        record.schema_version = SCHEMA_VERSION;
        record.updated_at = now;
        record.validate()?;
        fsio::ensure_dir(&self.root.join(super::CHARACTERS_DIR))?;
        fsio::write_json(&self.character_record_path(account.0), &record, MAX_PLAYER_CHARACTER_BYTES)?;
        Ok((record, true))
    }

    /// The character-progress record that `account`'s player-character points at for `game`, with the character
    /// the pointer names (the slot). `None` when the account has no player-character record, the record has no
    /// pointer to a character-progress record for `game`, or nothing was stored yet (the caller then starts from
    /// its game's own starting values, as it always has). The record found is **the one the store has always filed
    /// under (account, game)**: following the pointer moves nothing and re-keys nothing, so a build that does not
    /// know pointers reads the same file. The pointer's character and the record's own can differ (see
    /// [`LinkedProgress::in_sync`]); that is for the caller to decide, the store never rewrites either.
    pub fn linked_character_progress(&self, account: AccountId, game: GameId) -> Result<Option<LinkedProgress>> {
        let Some(character) = self.get_player_character(account)? else {
            return Ok(None);
        };
        let Some(ProgressRef::Character { character: expected_character, .. }) = character.game(game).and_then(GameCharacter::character_ref) else {
            return Ok(None);
        };
        let expected_character = *expected_character;
        Ok(self.read_character_progress(account, game)?.map(|record| LinkedProgress { record, expected_character }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CharacterProgress;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    /// Run `work` on another thread and fail unless it finishes while the caller still holds the writer lock.
    fn finishes_promptly<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        rx.recv_timeout(Duration::from_secs(20)).expect("the call was blocked by the writer lock it must not need")
    }

    fn legacy(_: &LegacyView<'_>) -> GameCharacter {
        GameCharacter { slot: Some(1), progress: GameCharacter::pointers(GameId::Ff7, Some(1)), ..GameCharacter::default() }
    }

    /// The documented exception to "a login never waits for a write" is only the ADDING of the record: reading
    /// one and following its pointer take no writer lock, and neither does asking for an entry that is there.
    #[test]
    fn reading_the_record_and_following_its_pointer_never_wait_for_the_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        let reg = store.register("Wedge", [1, 2, 3], "default", None, 5).unwrap();
        store.ensure_player_character(reg.account_id, GameId::Ff7, 6, legacy).unwrap();
        store.put_character_progress(&CharacterProgress::new(reg.account_id, GameId::Ff7, 1, 7, 6)).unwrap();

        let held = store.writer();
        let (s, account) = (Arc::clone(&store), reg.account_id);
        finishes_promptly(move || {
            assert!(s.get_player_character(account).unwrap().is_some());
            let linked = s.linked_character_progress(account, GameId::Ff7).unwrap().unwrap();
            assert_eq!((linked.record.level, linked.in_sync()), (7, true));
            let (_, added) = s.ensure_player_character(account, GameId::Ff7, 9, |_| panic!("already there")).unwrap();
            assert!(!added);
        });
        drop(held);
    }

    /// And adding one does wait (it is a write): the lock is real, not skipped.
    #[test]
    fn adding_the_record_waits_for_the_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        let reg = store.register("Wedge", [1, 2, 3], "default", None, 5).unwrap();
        let held = store.writer();
        let (tx, rx) = mpsc::channel();
        let (s, account) = (Arc::clone(&store), reg.account_id);
        let worker = std::thread::spawn(move || {
            let result = s.ensure_player_character(account, GameId::Ff7, 6, legacy).unwrap();
            let _ = tx.send(result.1);
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "it must wait for the writer lock");
        drop(held);
        assert!(rx.recv_timeout(Duration::from_secs(20)).expect("it finishes once the lock is free"));
        worker.join().unwrap();
    }
}
