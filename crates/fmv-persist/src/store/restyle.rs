//! Changing the look of an existing account's character: [`Store::restyle_player_character`], and, read-only, what
//! it would work on ([`Store::preview_player_character`]). Both are for an operator who has the server stopped
//! (`fmv-characters` of `fmv-server` is the tool); the store has no opinion on which bodies a game offers, so the
//! caller passes the look and the game's own rules decide what it is (the same split as
//! [`Store::ensure_player_character`], whose migration closure this takes too).
//!
//! # What a restyle changes, and what it never does
//!
//! Exactly three values of `characters/<account>.json`, and nothing else on the disk:
//!
//! * `identity.body`: the game-agnostic body choice,
//! * `identity.tint`: the colour, only when the change names one,
//! * the game's `binding`: how that game draws the character.
//!
//! It never changes the **slot**, the **progress pointers**, the identity's **name**, the **origin**, the
//! **created_at** time, any other game's entry or any other file: no avatar profile, account record, index entry,
//! character-progress record (level, experience, gil, AP, items, limit gauge) or story blob is written (the account's
//! own records are only READ, to build a record that is missing). The one file it writes is the record itself,
//! atomically (the old record or the new one is on the disk, never a torn one), under the store's writer lock.
//!
//! # An account without a record yet
//!
//! An account of a v6 or v7 server has no player-character record until its first login with a v8 server. A restyle
//! of such an account first builds the record **exactly as that login would** (the same closure, the same
//! inputs: the account's name, its avatar profile and its character-progress record; identity from the avatar
//! profile, no body, origin [`CharacterOrigin::Migrated`], the same timestamps), then applies the look change
//! to it, and writes the result in ONE step: the intermediate record is never on the disk, so a crash leaves the
//! account as it was or restyled, never half-migrated. The account's slot is then whatever the migration closure
//! decides, which is what the server would have given it: the restyle does not choose it.
//!
//! A record that exists but has no entry for the game (a character that has played another game only) gains the
//! entry the same way, and its other games' entries are left as they are. A record that has the game's entry is
//! **not rebuilt**: the closure is not called.
//!
//! # What it refuses
//!
//! Each of these leaves everything on the disk as it was ([`RestyleError`]): an unknown account; a record that
//! cannot be read (damaged, from a newer build, oversized: it is never replaced by a default behind anybody's
//! back); an account that has neither a record nor an avatar profile (there is nothing to build its character
//! from: the server gives an account its avatar at its first login, before the record); a look or an entry the
//! record's own checks refuse (sizes, characters, pointers: see [`PlayerCharacter`]).
//!
//! A change that would leave the record exactly as it is (the look is already this one, and nothing had to be
//! added) writes nothing: `updated_at` moves only when something does. A dry run does everything but the write
//! and returns the same report.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fmv_core::{AccountId, GameId};
use thiserror::Error;

use super::{Store, CHARACTERS_DIR};
use crate::character::{CharacterOrigin, GameCharacter, Identity, LegacyView, PlayerCharacter, MAX_PLAYER_CHARACTER_BYTES};
use crate::error::PersistError;
use crate::fsio;
use crate::model::{game_key, SCHEMA_VERSION};

/// The look a character is changed to: the only things a restyle may change (see the module documentation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restyle {
    /// The game-agnostic body choice, stored as `identity.body` (the game's own archetype id).
    pub body: String,
    /// How the game draws the character from now on, stored as the game's `binding` (an adapter-defined
    /// appearance reference, which the record's checks bound in size and characters).
    pub binding: String,
    /// The colour, stored as `identity.tint`. `None` keeps the stored colour.
    pub tint: Option<[u8; 3]>,
}

/// What a restyle had to add to the account before it could change the look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestyleAdded {
    /// Nothing: the record already had an entry for the game, and it was not rebuilt.
    Nothing,
    /// The whole record: the account predates player-character records (what its first login would have added).
    Record,
    /// The game's entry only: the record exists (the character has played another game) but had none for this game.
    Entry,
}

/// What a restyle did (or, in a dry run, would do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestyleReport {
    pub account: AccountId,
    pub game: GameId,
    /// What had to be added before the look could change.
    pub added: RestyleAdded,
    /// The record before the look change: the stored one, or, when `added` is not [`RestyleAdded::Nothing`], the record
    /// the account's first login would have made (stamped with the `now` of this call, as that login would).
    pub before: PlayerCharacter,
    /// The record after the change: what was written, or what would be (see `written`). Its slot, pointers, name,
    /// origin and `created_at` are `before`'s.
    pub after: PlayerCharacter,
    /// The record file this call writes (or would write): `characters/<account>.json` under the store's root.
    pub path: PathBuf,
    /// This was a dry run: nothing was written.
    pub dry_run: bool,
    /// `path` was written by this call. False for a dry run and for a change that leaves the record as it is.
    pub written: bool,
}

impl RestyleReport {
    /// Did the body, the colour or the game's binding change (or does it differ from what the migration builds)?
    pub fn look_changed(&self) -> bool {
        self.before.identity.body != self.after.identity.body
            || self.before.identity.tint != self.after.identity.tint
            || self.before.binding(self.game) != self.after.binding(self.game)
    }
}

/// Why a restyle (or a preview) was refused. Nothing was written in any of these cases.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RestyleError {
    /// The account does not exist.
    #[error("unknown account {}", .0.0)]
    UnknownAccount(AccountId),

    /// No record and no avatar profile: nothing to build the character from.
    #[error(
        "account {} has neither a player-character record nor an avatar profile, so there is nothing to build its \
         character from; log in with it once on a v8 server (which gives it an avatar and then its record) and \
         restyle it afterwards",
        .0.0
    )]
    NothingToMigrateFrom(AccountId),

    /// The record exists and cannot be read (damaged, from a newer build, oversized, or for another account). It
    /// is left exactly as it is.
    #[error("the player-character record cannot be read, so it is left exactly as it is: {0}")]
    UnreadableRecord(#[source] PersistError),

    /// Any other store failure (a record the migration reads is unreadable, the result breaks the record's own
    /// checks, the write failed, ...).
    #[error(transparent)]
    Store(#[from] PersistError),
}

impl Store {
    /// Change the look of `account`'s character in `game` to `change`, creating the record from the legacy data
    /// first if the account has none (see the module documentation for exactly what is and is not changed, what
    /// is refused and the creation rule). Takes the writer lock, reads the record under it and writes only
    /// `characters/<account>.json`, or, with `dry_run`, nothing at all: the report is the same.
    ///
    /// `legacy` is the migration closure of [`Store::ensure_player_character`] and runs under the same conditions:
    /// at most once, while the writer lock is held, and not at all when the record already has an entry for `game`.
    pub fn restyle_player_character<F>(
        &self,
        account: AccountId,
        game: GameId,
        change: &Restyle,
        now: u64,
        dry_run: bool,
        legacy: F,
    ) -> Result<RestyleReport, RestyleError>
    where
        F: FnOnce(&LegacyView<'_>) -> GameCharacter,
    {
        let _writers = self.writer()?;
        let (before, added) = self.plan_player_character(account, game, now, legacy)?;

        let mut after = before.clone();
        after.identity.body.clone_from(&change.body);
        if let Some(tint) = change.tint {
            after.identity.tint = tint;
        }
        let Some(entry) = after.games.get_mut(&game_key(game)) else {
            // `plan_player_character` guarantees the entry; this keeps the function total.
            return Err(PersistError::InvalidField { field: "games", reason: format!("the record has no entry for {}", game.key()) }.into());
        };
        entry.binding = Some(change.binding.clone());

        let looks_the_same = after.identity == before.identity && after.binding(game) == before.binding(game);
        let must_write = added != RestyleAdded::Nothing || !looks_the_same;
        if must_write {
            after.schema_version = SCHEMA_VERSION;
            after.updated_at = now;
        }
        after.validate()?;

        let path = self.character_record_path(account.0);
        let written = must_write && !dry_run;
        if written {
            fsio::ensure_dir(&self.root.join(CHARACTERS_DIR))?;
            fsio::write_json(&path, &after, MAX_PLAYER_CHARACTER_BYTES)?;
        }
        Ok(RestyleReport { account, game, added, before, after, path, dry_run, written })
    }

    /// The record [`Store::restyle_player_character`] would start from, and what it would have to add: the stored
    /// record, or the one the first login of an account that has none would make (built by `legacy` from the
    /// account's own records, with this `now`). Writes nothing.
    ///
    /// An account that has neither a record nor an avatar profile is refused ([`RestyleError::NothingToMigrateFrom`]):
    /// the server gives an account its avatar at login, before it makes the record.
    pub fn preview_player_character<F>(&self, account: AccountId, game: GameId, now: u64, legacy: F) -> Result<(PlayerCharacter, RestyleAdded), RestyleError>
    where
        F: FnOnce(&LegacyView<'_>) -> GameCharacter,
    {
        let _writers = self.writer()?;
        self.plan_player_character(account, game, now, legacy)
    }

    /// The record with an entry for `game`, as it is stored or as the account's first login would make it. The
    /// construction is [`Store::ensure_player_character`]'s, field by field (a test compares the two). The caller
    /// holds the writer lock; nothing is written.
    fn plan_player_character<F>(&self, account: AccountId, game: GameId, now: u64, legacy: F) -> Result<(PlayerCharacter, RestyleAdded), RestyleError>
    where
        F: FnOnce(&LegacyView<'_>) -> GameCharacter,
    {
        let existing = self.get_player_character(account).map_err(|e| match e {
            PersistError::UnknownAccount(account) => RestyleError::UnknownAccount(account),
            other => RestyleError::UnreadableRecord(other),
        })?;
        let key = game_key(game);
        if let Some(record) = &existing {
            if record.games.contains_key(&key) {
                return Ok((record.clone(), RestyleAdded::Nothing));
            }
        }

        let name = self.load_account(account.0)?.display_name;
        let avatar = self.avatar_for_account(account)?;
        let (mut record, added) = match (existing, &avatar) {
            (Some(record), _) => (record, RestyleAdded::Entry),
            (None, Some(avatar)) => (
                PlayerCharacter {
                    schema_version: SCHEMA_VERSION,
                    account_id: account,
                    identity: Identity { name: avatar.display_name.clone(), tint: avatar.tint, body: String::new() },
                    games: BTreeMap::new(),
                    origin: CharacterOrigin::Migrated,
                    created_at: now,
                    updated_at: now,
                },
                RestyleAdded::Record,
            ),
            (None, None) => return Err(RestyleError::NothingToMigrateFrom(account)),
        };
        let progress = self.read_character_progress(account, game)?;
        let entry = legacy(&LegacyView { name: &name, avatar: avatar.as_ref(), progress: progress.as_ref() });
        record.games.insert(key, entry);
        record.schema_version = SCHEMA_VERSION;
        record.updated_at = now;
        record.validate()?;
        Ok((record, added))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    fn change() -> Restyle {
        Restyle { body: "sturdy".into(), binding: "ff7/body/sturdy".into(), tint: None }
    }

    fn legacy(_: &LegacyView<'_>) -> GameCharacter {
        GameCharacter { slot: Some(1), progress: GameCharacter::pointers(GameId::Ff7, Some(1)), ..GameCharacter::default() }
    }

    /// A restyle is a write: it waits for the writer lock like every other one (so it cannot interleave with the
    /// record being added by a login, or with a save), and so does the preview that reads under it.
    #[test]
    fn restyling_and_previewing_wait_for_the_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        let reg = store.register("Wedge", [1, 2, 3], "default", Some((GameId::Ff7, "ff7/tifa")), 5).unwrap();
        let held = store.writer().unwrap();
        let (tx, rx) = mpsc::channel();
        let (s, account) = (Arc::clone(&store), reg.account_id);
        let worker = std::thread::spawn(move || {
            let (_, added) = s.preview_player_character(account, GameId::Ff7, 6, legacy).unwrap();
            let report = s.restyle_player_character(account, GameId::Ff7, &change(), 6, false, legacy).unwrap();
            let _ = tx.send((added, report.written));
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "they must wait for the writer lock");
        drop(held);
        assert_eq!(rx.recv_timeout(Duration::from_secs(20)).expect("they finish once the lock is free"), (RestyleAdded::Record, true));
        worker.join().unwrap();
    }

    #[test]
    fn the_errors_say_which_account_and_what_to_do() {
        assert_eq!(RestyleError::UnknownAccount(AccountId(7)).to_string(), "unknown account 7");
        let text = RestyleError::NothingToMigrateFrom(AccountId(7)).to_string();
        assert!(text.starts_with("account 7 has neither") && text.contains("log in with it once"), "{text}");
    }
}
