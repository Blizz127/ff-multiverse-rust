//! Account login and registration over the persistent store (`fmv_persist::Store`).
//!
//! A connection proves who it is with `Hello::auth`: a token (log in) or a display name
//! (register). Everything a player looks like in the world and fights as comes from the STORED player
//! character (`fmv_persist::PlayerCharacter`), never from what the client sends after registration: the
//! client's `Hello::character` (colour, body, slot) only seeds a NEW character (see `character` for what
//! the game offers).
//!
//! Every function here that takes a `Store` does blocking disk I/O. Async callers must run them
//! under `spawn_blocking` (see `conn`).
//!
//! A registered display name is unique and cannot be recovered without its token, so the token
//! is returned in [`Login::issued_token`] exactly once and the server keeps only its hash.
//!
//! # Registration cannot fail after the name is taken
//!
//! 1. [`validate_registration`] checks everything the client chose (display name, appearance)
//!    without touching the store, so a connection can be refused for it BEFORE it takes a place in
//!    the registration rate limits (`limits`) and before anything is written.
//! 2. [`register`] creates the account and its avatar in ONE store call; [`register_with_rules`]
//!    adds the authored starting progress to that same commit. Both build the reply from its result without
//!    reading anything else. So once the name is committed there is nothing left that can fail
//!    before the token is in the caller's hands.
//! 3. The caller delivers the token immediately. If the `Welcome` that carries it is never
//!    written, [`release_unclaimed`] gives the account (and so the name) back: nobody holds a
//!    token for it. If it was written, the account stays until the client answers it: its first
//!    message after the `Welcome` is recorded with [`claim_delivered`], which is what keeps the
//!    store's orphan tools (`Store::unclaimed_accounts`, `fmv-accounts release-older-than`) away from
//!    a player who registered, played, and has not logged in again.
//!
//! # The player character
//!
//! A registration creates the account, its avatar profile (a copy of the identity that a build before protocol v8
//! reads) and the player-character record in one store call (`Store::register_with_character`). A token login
//! of an account that predates the record adds it first, from what the account already has and without changing
//! any of it (`Store::ensure_player_character`, with `character::legacy_entry`): so a v6 or v7 account keeps its
//! numbers, its look and the character it was fighting as. The character's **slot** (not its look) decides who
//! it fights as, and the progress record is found through the character's pointer; if the record holds another
//! character than the slot (a rolled-back v7 saved it by look) the slot wins and the mismatch is logged.
//!
//! With battle rules available, registration stores the game's starting resources once. Login of an older
//! account with no progress stores its historical zero-gil defaults before it enters the world. Existing
//! records, including a missing inventory (`None`) or an explicitly empty one, are read unchanged.
//!
//! # Appearance
//!
//! The binding is broadcast to every player and names a model, so it is only ever one of the stable ids the hosted
//! game's adapter knows (`character::canonical_binding`); anything else (a path, a different game's id, an
//! enormous string) is shown as no look. The same filter runs on what the store returns, so a hand-edited record
//! cannot put an arbitrary string in front of other players either.

use fmv_core::{AccountId, AvatarAppearance, GameId, NameError, PhaseKey};
use fmv_persist::{CharacterProgress, NewCharacterProgress, NewPlayerCharacter, PersistError, Store};
use fmv_protocol::{BattleRules, CharacterCard, NewCharacter, RejectCode, Secret};

use crate::character::{self, Creation, Refusal};
use crate::progress::{self, Progress, ProgressSeed};

/// Base archetype of a new avatar (every adapter maps archetypes to its closest model).
pub const DEFAULT_ARCHETYPE: &str = "default";

/// Longest `appearance_ref` (in chars) the server keeps. Together with the 24-char display name
/// this bounds every entity snapshot, and therefore the size of the `AreaSnapshot` frame. It is
/// core's limit, which the store enforces in the same unit (characters), so a ref the server keeps
/// can never be refused by the store.
pub const MAX_APPEARANCE_REF: usize = AvatarAppearance::MAX_APPEARANCE_REF;

/// Progression blobs up to this long are the `PhaseKey` as they are.
pub const MAX_VERBATIM_PHASE_BYTES: usize = 32;
/// First byte of a digest `PhaseKey` (see [`phase_key`]).
const PHASE_DIGEST_TAG: u8 = 0xB3;

/// A successful login or registration.
#[derive(Debug)]
pub struct Login {
    pub account: AccountId,
    /// What other players see, from the stored player character and for the hosted game: its name and colour and
    /// its binding.
    pub avatar: AvatarAppearance,
    /// The account's story progression for the hosted game: the stored blob, which only the game's
    /// adapter reads (empty for a new account and for one that never advanced; see
    /// [`kept_progression`] for what becomes of an enormous one). The world turns it into the phase of
    /// each area the player stands in.
    pub progression: Vec<u8>,
    /// `Some` only when this call registered the account: the token, to be sent once.
    pub issued_token: Option<String>,
    /// The character it fights as (its combat slot) and what the store holds of its character progress
    /// (initialized during registration when the server has rules). Separate from `progression`: see `progress`.
    pub progress: ProgressSeed,
    /// What the player is told about its own character in `Welcome` (identity, binding, slot).
    pub card: CharacterCard,
    /// Where its player last stood in the hosted game (MMO-style saving: see `world::resume`). `None` for a new
    /// account, an account of an older build, and a record that cannot be read (said in the log; the account then joins
    /// as it always did).
    pub location: Option<fmv_persist::SavedLocation>,
}

/// Why a login failed.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    /// The client's fault. The code and text go to the client in a `Reject`: the text never
    /// echoes anything the client sent and never says whether a token or a name exists (the name
    /// being taken is the one thing a registration has to say).
    Refused(RejectCode, &'static str),
    /// The server could not read or write its store. The client gets a generic `Reject`; the
    /// cause (which may name local paths) is for the log only.
    Internal(String),
}

impl LoginError {
    /// The code for the client's `Reject`.
    pub fn code(&self) -> RejectCode {
        match self {
            LoginError::Refused(code, _) => *code,
            LoginError::Internal(_) => RejectCode::Internal,
        }
    }

    /// The text for the client's `Reject`.
    pub fn client_text(&self) -> &str {
        match self {
            LoginError::Refused(_, text) => text,
            LoginError::Internal(_) => "the account store failed; try again later",
        }
    }
}

/// Why a display name is not admitted, in words for the player (all of them `InvalidName`).
fn name_refusal(why: NameError) -> LoginError {
    LoginError::Refused(
        RejectCode::InvalidName,
        match why {
            NameError::Empty => "display name must not be empty",
            NameError::MixedScripts => {
                "display name mixes writing systems (for example Latin with Cyrillic or Greek); use letters of one writing system"
            }
            NameError::UnsupportedCharacters => "display name uses characters from an unsupported script",
        },
    )
}

fn map_store_error(e: PersistError) -> LoginError {
    match e {
        PersistError::InvalidToken => LoginError::Refused(RejectCode::InvalidToken, "invalid or unknown token"),
        // `validate_registration` says why before the store is reached; this is the store's own
        // check of the same rule.
        PersistError::InvalidName => LoginError::Refused(
            RejectCode::InvalidName,
            "display name is not allowed (it needs visible characters from a single writing system)",
        ),
        PersistError::NameTaken(_) => LoginError::Refused(RejectCode::NameTaken, "that display name is already taken"),
        // The store holds as many accounts as it was configured to: logins are unaffected.
        PersistError::AccountLimitReached { .. } => {
            LoginError::Refused(RejectCode::RegistrationClosed, "this server is not accepting new accounts")
        }
        other => LoginError::Internal(other.to_string()),
    }
}

/// The `PhaseKey` for a progression blob. It is cloned for every player on every placement refresh
/// and serialized into `InstancePlacement` as a JSON array (up to four bytes per byte), so it must
/// be small: a blob of about 64 KiB would make that frame exceed `MAX_FRAME` and the account could
/// never log in again.
///
/// Blobs of up to [`MAX_VERBATIM_PHASE_BYTES`] bytes are the key itself (an empty blob is the
/// single free-roam phase, as always). A longer one becomes `[PHASE_DIGEST_TAG] ++ blake3(blob)`:
/// 33 bytes, equal exactly when the blobs are equal, and never equal to a verbatim key because it
/// is longer than any of them. A phase is an identity to compare, not data to read back.
pub fn phase_key(blob: Vec<u8>) -> PhaseKey {
    if blob.len() <= MAX_VERBATIM_PHASE_BYTES {
        return PhaseKey(blob);
    }
    let mut key = Vec::with_capacity(1 + 32);
    key.push(PHASE_DIGEST_TAG);
    key.extend_from_slice(blake3::hash(&blob).as_bytes());
    PhaseKey(key)
}

/// Longest progression blob the world keeps per player as it is. This accommodates FF7's bounded
/// persistent banks; anything longer is kept as its digest
/// ([`phase_key`]), an unreadable blob of 33 bytes with the same identity, so that a hand-edited
/// 256 KiB record cannot cost 256 KiB per connected player.
pub const MAX_PROGRESSION_BYTES: usize = fmv_protocol::MAX_STORY_PROGRESS_BYTES;

/// The progression the world keeps for a stored blob: the blob itself, or, when it is longer than
/// [`MAX_PROGRESSION_BYTES`], its digest. Neither is ever written back by the story code (an
/// unreadable blob is never advanced or overwritten).
pub fn kept_progression(blob: Vec<u8>) -> Vec<u8> {
    if blob.len() <= MAX_PROGRESSION_BYTES {
        blob
    } else {
        phase_key(blob).0
    }
}

/// A registration whose every client-chosen part has been checked (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The display name, cleaned and admitted (`AvatarAppearance::validate_new_name`).
    name: String,
    /// The identity colour the client chose.
    tint: [u8; 3],
    /// What the hosted game makes of the client's choices: the body, the binding and the slot (only
    /// `validate_registration` builds one, so it is never a refused request).
    creation: Creation,
}

#[cfg(test)]
impl Registration {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tint(&self) -> [u8; 3] {
        self.tint
    }

    pub fn creation(&self) -> &Creation {
        &self.creation
    }
}

/// Why a new character's choices were refused, as the client is told.
fn character_refusal(why: Refusal) -> LoginError {
    LoginError::Refused(RejectCode::BadRequest, why.client_text())
}

/// Check what a client chose for a new account, without touching the store: the name, and what the hosted game
/// offers of the body and the combat slot (see `character::plan_creation`: an unknown body is the default body, a
/// slot the game does not offer to a new character is refused).
pub fn validate_registration(game: GameId, display_name: &str, requested: &NewCharacter) -> Result<Registration, LoginError> {
    let name = AvatarAppearance::validate_new_name(display_name).map_err(name_refusal)?;
    let creation = character::plan_creation(game, requested).map_err(character_refusal)?;
    Ok(Registration { name, tint: requested.tint, creation })
}

/// Log in with a token. `tint` is only used to give an account that has no avatar one.
///
/// An account that predates the player-character record gets one here, added without changing anything it has
/// (module docs), and its progress is found through the character's pointer: a record that cannot be read
/// (damaged, from a newer build) fails the login like a story blob that cannot be read does, and is never
/// replaced by a default, which would later overwrite it.
pub fn login_with_token(store: &Store, game: GameId, token: &Secret, tint: [u8; 3], now: u64) -> Result<Login, LoginError> {
    login_with_token_inner(store, game, token, tint, now, None)
}

/// Login with authoritative resource initialization. A missing legacy record is inserted with the
/// same zero-gil defaults older builds showed; a stored record is never changed by this path.
pub fn login_with_token_and_rules(store: &Store, game: GameId, token: &Secret, tint: [u8; 3], now: u64, rules: &dyn BattleRules) -> Result<Login, LoginError> {
    login_with_token_inner(store, game, token, tint, now, Some(rules))
}

fn login_with_token_inner(store: &Store, game: GameId, token: &Secret, tint: [u8; 3], now: u64, rules: Option<&dyn BattleRules>) -> Result<Login, LoginError> {
    let account = store.authenticate(token.expose()).map_err(map_store_error)?;
    if store.avatar_for_account(account).map_err(map_store_error)?.is_none() {
        // An account without an avatar (made by other tooling): give it one now.
        store.create_avatar(account, tint, DEFAULT_ARCHETYPE, now).map_err(map_store_error)?;
    }
    let progression = store.get_progression(account, game).map_err(map_store_error)?;
    let (record, added) =
        store.ensure_player_character(account, game, now, |view| character::legacy_entry(game, view)).map_err(map_store_error)?;
    if added {
        tracing::info!(
            account = account.0,
            game = game.key(),
            slot = ?record.slot(game),
            binding = ?record.binding(game),
            "player character added to an account that predates it (nothing the account had was changed)"
        );
    }
    // Follow the pointer to the progress record. A game without a pointer reads the record of (account, game) all
    // the same: that is where it always was.
    let mut stored: Option<CharacterProgress> = match store.follow_character_progress(&record, game).map_err(map_store_error)? {
        Some(linked) => {
            if !linked.in_sync() {
                tracing::warn!(
                    account = account.0,
                    slot = linked.expected_character,
                    stored_character = linked.record.character,
                    "the stored progress names another character than the player's slot (a rolled-back build saved it by look); the slot wins and the next save writes it back"
                );
            }
            Some(linked.record)
        }
        None => store.get_character_progress(account, game).map_err(map_store_error)?,
    };
    let slot = record.slot(game).unwrap_or(progress::DEFAULT_CHARACTER);
    if stored.is_none() {
        if let Some(rules) = rules {
            stored = Some(store.ensure_character_progress(account, game, || {
                let mut initial = progress::fresh(rules, slot).to_record(account, game, now);
                initial.inventory = Some(crate::inventory::Inventory::resolve(rules, None).entries().to_vec());
                initial
            }).map_err(map_store_error)?);
        }
    }
    let progress = ProgressSeed {
        character: slot,
        stored: stored.as_ref().map(Progress::from_record),
        // `None` (a record of an older build, or no record) is "never stored": the starting stock.
        inventory: stored.as_ref().and_then(|record| record.inventory.clone()),
    };
    let location = store.get_location(account, game).unwrap_or_else(|e| {
        tracing::warn!(account = account.0, error = %e, "the saved spot cannot be read; the account joins where everyone joins");
        None
    });
    Ok(Login {
        account,
        avatar: character::shown(game, &record),
        progression: kept_progression(progression.blob),
        issued_token: None,
        progress,
        card: character::card(game, &record),
        location,
    })
}

/// Create the account, its avatar and its player character in one store call. `on_created` runs the moment the
/// account exists (the registration rate limits count it then; a request that never gets this far counts for
/// nothing). Nothing after that can fail: the reply is built from what the store returned.
///
/// This variant has no battle rules, so it leaves character progress unstored. A brand-new account
/// has no story progression (the empty blob: a new account to every game), and nothing else is read.
pub fn register(store: &Store, game: GameId, registration: Registration, now: u64, on_created: impl FnOnce()) -> Result<Login, LoginError> {
    register_inner(store, game, registration, now, on_created, None)
}

/// Atomically create the identity and its authored starting resources before issuing the token.
/// The returned progress is the committed record, with an explicit starting inventory.
pub fn register_with_rules(store: &Store, game: GameId, registration: Registration, now: u64, on_created: impl FnOnce(), rules: &dyn BattleRules) -> Result<Login, LoginError> {
    register_inner(store, game, registration, now, on_created, Some(rules))
}

fn register_inner(store: &Store, game: GameId, registration: Registration, now: u64, on_created: impl FnOnce(), rules: Option<&dyn BattleRules>) -> Result<Login, LoginError> {
    let Registration { name, tint, creation } = registration;
    let initial = rules.map(|rules| {
        let progress = progress::new_character(rules, creation.entry.slot.unwrap_or(progress::DEFAULT_CHARACTER));
        NewCharacterProgress { game, character: progress.character, level: progress.level, exp: progress.exp, ap: progress.ap,
            gil: progress.gil, limit: progress.limit, inventory: Some(crate::inventory::Inventory::resolve(rules, None).entries().to_vec()) }
    });
    let wanted = NewPlayerCharacter::for_game(creation.body.clone(), game, creation.entry.clone());
    let (registered, initial) = store
        .register_with_character_and_progress(&name, tint, DEFAULT_ARCHETYPE, creation.v7_look.map(|look| (game, look)), Some(wanted), initial, now)
        .map_err(map_store_error)?;
    on_created();
    // The record the store returned is what was written. (It always returns one when it was given one; the
    // fallback only keeps this function total: a registration that has committed cannot fail.)
    let (avatar, card) = match &registered.character {
        Some(record) => (character::shown(game, record), character::card(game, record)),
        None => (AvatarAppearance::new(name.clone(), tint, None), CharacterCard::plain(name, tint)),
    };
    let slot = registered.character.as_ref().and_then(|record| record.slot(game));
    Ok(Login {
        account: registered.account_id,
        avatar,
        progression: Vec::new(),
        issued_token: Some(registered.token),
        progress: ProgressSeed { character: slot.unwrap_or(progress::DEFAULT_CHARACTER), stored: initial.as_ref().map(Progress::from_record),
            inventory: initial.and_then(|record| record.inventory) },
        card,
        location: None,
    })
}

/// Whose character a login token is, for a request that is not a login (a delete: `ClientMsg::DeleteCharacter`). The
/// refusal is what a login with a token the server does not know gets (`InvalidToken`), which a client reads as "this
/// login is not on the server (any more)".
pub fn account_of_token(store: &Store, token: &Secret) -> Result<AccountId, LoginError> {
    store.authenticate(token.expose()).map_err(map_store_error)
}

/// What deleting a character did, for the reply and the log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    /// The character's display name, free again.
    pub name: String,
    /// The trash entry that holds its records (a directory name under the store's `trash/`).
    pub entry: String,
    /// How many record files it holds.
    pub files: usize,
    /// How many of them the move could not finish (0 normally; the store finishes them the next time it opens).
    pub unfinished: usize,
}

/// Delete the character of `account`: the store moves its records into a timestamped entry of its trash, frees its name
/// and removes it from every index (`Store::delete_account`; nothing of any other account is touched). The caller has
/// checked that the account is not in the world (see `deletion`). An account that is not there (a second delete, a
/// delete that raced one) is `InvalidToken`, like any login with a token the server does not know.
pub fn delete_character(store: &Store, account: AccountId, now: u64) -> Result<Deletion, LoginError> {
    match store.delete_account(account, now) {
        Ok(deleted) => {
            // The saved spot is no record the trash takes (MMO-style saving: `world::resume`): forget it, so nothing of
            // the deleted character stays in the live directories. A late spot write is refused for a deleted account.
            if let Err(e) = store.forget_locations(account) {
                tracing::warn!(account = account.0, error = %e, "the deleted character's saved spot could not be removed");
            }
            Ok(Deletion {
            name: deleted.display_name,
            entry: deleted.entry,
            files: deleted.files.len(),
            unfinished: deleted.unfinished.len(),
            })
        }
        Err(PersistError::UnknownAccount(_)) => Err(LoginError::Refused(RejectCode::InvalidToken, "invalid or unknown token")),
        Err(e) => Err(LoginError::Internal(e.to_string())),
    }
}

/// Record that the client of a registration answered the `Welcome` that carried its new token (its
/// first message after it): the token has been delivered as far as the server can tell, so the
/// account is not an orphan and [`release_unclaimed`], `Store::unclaimed_accounts` and the
/// `fmv-accounts` tool leave it alone. A registering session never logs in with its token, so without
/// this every player who registered and has not logged in again would look like a registration whose
/// token never arrived. `Ok(true)` if this recorded it, `Ok(false)` if it already was; `Err` is a text
/// for the log (the account is unknown or the store failed).
pub fn claim_delivered(store: &Store, account: AccountId) -> Result<bool, String> {
    store.mark_claimed(account).map_err(|e| e.to_string())
}

/// What [`release_unclaimed`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// The account was deleted: its name is free again and its token is void.
    Released,
    /// There was nothing to release: the account is unknown, its token has been used to log in, or
    /// it has progression. Whoever holds it keeps it.
    NotReleasable,
    /// The store failed. The account (and its name) stays; the text is for the log.
    Failed(String),
}

/// Give back an account that a registration created but whose token never reached its owner (the
/// connection died before the `Welcome` was written). Nobody holds the token, so deleting the
/// account loses nothing, and keeping it would burn the name for good. The store decides under
/// its own lock that the account has never been logged into (`Store::release_if_unclaimed`).
pub fn release_unclaimed(store: &Store, account: AccountId) -> Release {
    match store.release_if_unclaimed(account) {
        Ok(true) => Release::Released,
        Ok(false) => Release::NotReleasable,
        Err(e) => Release::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests;
