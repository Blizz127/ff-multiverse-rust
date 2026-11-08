//! Account login and registration over the persistent store (`fmv_persist::Store`).
//!
//! A connection proves who it is with `Hello::auth`: a token (log in) or a display name
//! (register). Everything a player looks like in the world comes from the STORED avatar profile,
//! never from what the client sends after registration: the client's `tint` and
//! `appearance_ref` only seed a NEW avatar.
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
//! 2. [`register`] creates the account and its avatar in ONE store call (`Store::register`: a
//!    single committed log entry), and builds the reply from what that call returned without
//!    reading anything else. So once the name is committed there is nothing left that can fail
//!    before the token is in the caller's hands.
//! 3. The caller delivers the token immediately. If the `Welcome` that carries it is never
//!    written, [`release_unclaimed`] gives the account (and so the name) back: nobody holds a
//!    token for it. If it was written, the account stays until the client answers it: its first
//!    message after the `Welcome` is recorded with [`claim_delivered`], which is what keeps the
//!    store's orphan tools (`Store::unclaimed_accounts`, `fmv-accounts release-older-than`) away from
//!    a player who registered, played, and has not logged in again.
//!
//! # Appearance
//!
//! `appearance_ref` is broadcast to every player and will one day name a model, so it is only
//! ever one of the stable ids the hosted game's adapter knows ([`clean_appearance_ref`]); anything
//! else (a path, a different game's id, an enormous string) is dropped and the avatar keeps the
//! default look. The same filter runs on what the store returns, so an old or hand-edited profile
//! cannot put an arbitrary string in front of other players either.

use fmv_core::{AccountId, AvatarAppearance, GameId, NameError, PhaseKey};
use fmv_persist::{AvatarProfile, PersistError, Store};
use fmv_protocol::{RejectCode, Secret};

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
    /// What other players see, from the stored profile and for the hosted game.
    pub avatar: AvatarAppearance,
    /// The account's story phase for the hosted game, taken from its progression (empty by
    /// default, so every player is in the single free-roam phase).
    pub phase: PhaseKey,
    /// `Some` only when this call registered the account: the token, to be sent once.
    pub issued_token: Option<String>,
    /// The character its look names and what the store holds of its character progress (nothing for
    /// a new account). Separate from `phase`: see `progress`.
    pub progress: ProgressSeed,
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

/// The stable appearance id of `game`'s adapter that `raw` names, or `None` if the game has no such
/// look. The ids are the ones persisted in avatar profiles; a game that lists none accepts none.
fn canonical_appearance(game: GameId, raw: &str) -> Option<&'static str> {
    match game {
        GameId::Ff7 => fmv_ff7::playable::resolve_appearance(raw).map(|model| model.appearance_ref),
        _ => None,
    }
}

/// What the server keeps of a client's `appearance_ref`: core's cleaning
/// (`AvatarAppearance::clean_appearance_ref`, the rule the store shares: control characters
/// dropped, at most 64 CHARACTERS, trimmed) and then the canonical id of one of the looks the
/// hosted `game` offers, else `None` (the default look). Never an error: an unknown look must not
/// cost anyone their registration.
pub fn clean_appearance_ref(game: GameId, raw: Option<&str>) -> Option<String> {
    let cleaned = AvatarAppearance::clean_appearance_ref(raw?)?;
    canonical_appearance(game, &cleaned).map(str::to_string)
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

/// A registration whose every client-chosen part has been checked (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The display name, cleaned and admitted (`AvatarAppearance::validate_new_name`).
    name: String,
    /// The canonical appearance id, if the client asked for a look the game offers.
    appearance: Option<String>,
}

#[cfg(test)]
impl Registration {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn appearance(&self) -> Option<&str> {
        self.appearance.as_deref()
    }
}

/// Check what a client chose for a new account, without touching the store.
pub fn validate_registration(
    game: GameId,
    display_name: &str,
    appearance_ref: Option<&str>,
) -> Result<Registration, LoginError> {
    let name = AvatarAppearance::validate_new_name(display_name).map_err(name_refusal)?;
    Ok(Registration { name, appearance: clean_appearance_ref(game, appearance_ref) })
}

/// Log in with a token. `tint` is only used to give an account that has no avatar one.
pub fn login_with_token(store: &Store, game: GameId, token: &Secret, tint: [u8; 3], now: u64) -> Result<Login, LoginError> {
    let account = store.authenticate(token.expose()).map_err(map_store_error)?;
    let profile = match store.avatar_for_account(account).map_err(map_store_error)? {
        Some(profile) => profile,
        // An account without an avatar (made by other tooling): give it one now.
        None => store.create_avatar(account, tint, DEFAULT_ARCHETYPE, now).map_err(map_store_error)?,
    };
    let progression = store.get_progression(account, game).map_err(map_store_error)?;
    // A record that cannot be read (damaged, from a newer build) fails the login like a story blob that
    // cannot be read does: it is never replaced by a default, which would later overwrite it.
    let stored = store.get_character_progress(account, game).map_err(map_store_error)?;
    let avatar = shown(game, &profile);
    let progress = ProgressSeed {
        character: progress::character_for_look(game, avatar.appearance_ref.as_deref()),
        stored: stored.as_ref().map(Progress::from_record),
        // `None` (a record of an older build, or no record) is "never stored": the starting stock.
        inventory: stored.as_ref().and_then(|record| record.inventory.clone()),
    };
    Ok(Login { account, avatar, phase: phase_key(progression.blob), issued_token: None, progress })
}

/// Create the account and its avatar in one store call. `on_created` runs the moment the account
/// exists (the registration rate limits count it then; a request that never gets this far counts
/// for nothing). Nothing after that can fail: the reply is built from what the store returned.
///
/// A brand-new account has no progression, so its phase is the default one and nothing else is
/// read.
pub fn register(
    store: &Store,
    game: GameId,
    registration: Registration,
    tint: [u8; 3],
    now: u64,
    on_created: impl FnOnce(),
) -> Result<Login, LoginError> {
    let Registration { name, appearance } = registration;
    let registered = store
        .register(&name, tint, DEFAULT_ARCHETYPE, appearance.as_deref().map(|look| (game, look)), now)
        .map_err(map_store_error)?;
    on_created();
    let avatar = shown(game, &registered.avatar);
    let progress = ProgressSeed::of(progress::character_for_look(game, avatar.appearance_ref.as_deref()), None);
    Ok(Login {
        account: registered.account_id,
        avatar,
        phase: PhaseKey::default(),
        issued_token: Some(registered.token),
        progress,
    })
}

/// What other players see of a stored profile. What is stored is not trusted: it reaches every
/// other player, so a look that is not one the game offers (a profile written by an older build or
/// by hand: a path, another game's id) is shown as the default look.
fn shown(game: GameId, profile: &AvatarProfile) -> AvatarAppearance {
    let mut avatar = profile.appearance_for(game);
    avatar.appearance_ref = avatar.appearance_ref.as_deref().and_then(|r| canonical_appearance(game, r)).map(str::to_string);
    avatar
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
mod tests {
    use super::*;
    use fmv_persist::Progression;
    use fmv_protocol::{Auth, Secret};
    use std::cell::Cell;

    const NOW: u64 = 1_000;

    /// Authenticate or register in one call. (`conn` does it in steps, so that the registration rate
    /// limits sit between validating and creating.) `tint` and `appearance_ref` are the look requested
    /// for a new avatar and are ignored when logging in with a token.
    fn login(
        store: &Store,
        game: GameId,
        auth: Auth,
        tint: [u8; 3],
        appearance_ref: Option<String>,
        now: u64,
    ) -> Result<Login, LoginError> {
        match auth {
            Auth::Token(token) => login_with_token(store, game, &token, tint, now),
            Auth::Register { display_name } => {
                let registration = validate_registration(game, &display_name, appearance_ref.as_deref())?;
                register(store, game, registration, tint, now, || {})
            }
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        (dir, store)
    }

    fn register_for(game: GameId, store: &Store, name: &str, tint: [u8; 3], appearance: Option<&str>) -> Result<Login, LoginError> {
        login(store, game, Auth::Register { display_name: name.into() }, tint, appearance.map(str::to_string), NOW)
    }

    fn register_ff7(store: &Store, name: &str, tint: [u8; 3], appearance: Option<&str>) -> Result<Login, LoginError> {
        register_for(GameId::Ff7, store, name, tint, appearance)
    }

    fn with_token(store: &Store, token: &str) -> Result<Login, LoginError> {
        login(store, GameId::Ff7, Auth::Token(Secret::new(token)), [9, 9, 9], Some("ignored".into()), NOW + 5)
    }

    #[test]
    fn registering_creates_the_account_and_avatar_and_issues_the_token_once() {
        let (_dir, store) = store();
        let reg = register_ff7(&store, "  Cloud\n", [10, 20, 30], Some("ff7/cloud")).unwrap();
        assert_eq!(reg.avatar.display_name, "Cloud", "the name is cleaned like everywhere else");
        assert_eq!(reg.avatar.tint, [10, 20, 30]);
        assert_eq!(reg.avatar.appearance_ref.as_deref(), Some("ff7/cloud"));
        assert_eq!(reg.phase, PhaseKey::default());
        let token = reg.issued_token.clone().expect("a registration issues a token");
        assert!(!token.is_empty());

        // The token logs the same account in again, and issues nothing the second time.
        let again = with_token(&store, &token).unwrap();
        assert_eq!(again.account, reg.account);
        assert!(again.issued_token.is_none());
        assert_eq!(again.avatar, reg.avatar);
    }

    #[test]
    fn after_registration_the_client_cannot_choose_its_look() {
        let (_dir, store) = store();
        let reg = register_ff7(&store, "Tifa", [1, 2, 3], Some("ff7/tifa")).unwrap();
        let token = reg.issued_token.unwrap();
        // The tint and appearance sent with a token login are ignored: the stored profile wins.
        let again = with_token(&store, &token).unwrap();
        assert_eq!(again.avatar.tint, [1, 2, 3]);
        assert_eq!(again.avatar.appearance_ref.as_deref(), Some("ff7/tifa"));
        assert_eq!(again.avatar.display_name, "Tifa");
    }

    #[test]
    fn the_appearance_override_is_per_game() {
        let (_dir, store) = store();
        let reg = register_ff7(&store, "Aerith", [4, 5, 6], Some("ff7/aerith")).unwrap();
        let token = reg.issued_token.unwrap();
        // The same account hosted by a different game has no override there.
        let other = login(&store, GameId::Fixture, Auth::Token(Secret::new(token)), [0; 3], None, NOW).unwrap();
        assert_eq!(other.avatar.appearance_ref, None);
        assert_eq!(other.avatar.display_name, "Aerith");
    }

    #[test]
    fn refusals_are_typed_and_say_nothing_about_which_tokens_or_names_exist() {
        let (_dir, store) = store();
        register_ff7(&store, "Barret", [0; 3], None).unwrap();
        // Name already taken (however it is written).
        for taken in ["Barret", "BARRET", "  barret ", "B\u{200B}arret"] {
            assert_eq!(
                register_ff7(&store, taken, [0; 3], None).unwrap_err(),
                LoginError::Refused(RejectCode::NameTaken, "that display name is already taken"),
                "{taken:?}"
            );
        }
        // Nothing visible left.
        for empty in ["", "   ", "\u{200B}\u{FEFF}", "\u{7}"] {
            assert_eq!(
                register_ff7(&store, empty, [0; 3], None).unwrap_err(),
                LoginError::Refused(RejectCode::InvalidName, "display name must not be empty"),
                "{empty:?}"
            );
        }
        // Unknown, malformed and oversized tokens are one and the same refusal.
        for bad in ["", "nope", "fmvt_0000", &"x".repeat(10_000)] {
            assert_eq!(
                with_token(&store, bad).unwrap_err(),
                LoginError::Refused(RejectCode::InvalidToken, "invalid or unknown token")
            );
        }
        // Refused registrations did not burn the name or create accounts.
        assert!(register_ff7(&store, "Yuffie", [0; 3], None).is_ok());
    }

    #[test]
    fn a_look_alike_name_is_refused_with_its_own_reason() {
        let (_dir, store) = store();
        // "Tifa" with a Cyrillic T: the classic impersonation.
        let err = register_ff7(&store, "\u{0422}ifa", [0; 3], None).unwrap_err();
        assert_eq!(err.code(), RejectCode::InvalidName);
        assert!(err.client_text().contains("writing systems"), "{}", err.client_text());
        // Another script entirely is a different reason, same code.
        assert_eq!(validate_registration(GameId::Ff7, "\u{0422}ifa", None).unwrap_err(), err);
        // And the whole thing happened before any account existed.
        assert!(register_ff7(&store, "Tifa", [0; 3], None).is_ok());
    }

    #[test]
    fn the_phase_comes_from_the_accounts_progression() {
        let (_dir, store) = store();
        let reg = register_ff7(&store, "Vincent", [0; 3], None).unwrap();
        let token = reg.issued_token.unwrap();
        store.put_progression(&Progression::new(reg.account, GameId::Ff7, vec![7, 7], NOW)).unwrap();
        assert_eq!(with_token(&store, &token).unwrap().phase, PhaseKey(vec![7, 7]));
        // Another game's progression is not this game's phase.
        store.put_progression(&Progression::new(reg.account, GameId::Fixture, vec![1], NOW)).unwrap();
        assert_eq!(with_token(&store, &token).unwrap().phase, PhaseKey(vec![7, 7]));
    }

    #[test]
    fn a_big_progression_blob_still_gives_a_small_phase_key_and_a_login_that_fits_a_frame() {
        // Verbatim up to the limit, a digest above it, and the two forms can never be equal.
        let at_limit = vec![9u8; MAX_VERBATIM_PHASE_BYTES];
        assert_eq!(phase_key(at_limit.clone()), PhaseKey(at_limit));
        assert_eq!(phase_key(vec![]), PhaseKey::default());
        let over = phase_key(vec![9u8; MAX_VERBATIM_PHASE_BYTES + 1]);
        assert_eq!(over.0.len(), 33);
        assert_eq!(over.0[0], PHASE_DIGEST_TAG);
        // Equal blobs, equal keys; different blobs (even one byte apart), different keys.
        let (a, b) = (vec![1u8; 5_000], {
            let mut b = vec![1u8; 5_000];
            b[4_999] = 2;
            b
        });
        assert_eq!(phase_key(a.clone()), phase_key(a.clone()));
        assert_ne!(phase_key(a.clone()), phase_key(b));
        // The digest of a blob is not the same key as the blob that equals the digest.
        let digest = phase_key(a.clone()).0;
        assert_ne!(phase_key(digest[1..].to_vec()), phase_key(a));

        // End to end: the biggest blob the store accepts (256 KiB) cannot lock the account out.
        let (_dir, store) = store();
        let reg = register_ff7(&store, "Hugo", [0; 3], None).unwrap();
        let token = reg.issued_token.unwrap();
        store
            .put_progression(&Progression::new(reg.account, GameId::Ff7, vec![0xAB; fmv_persist::MAX_BLOB_BYTES], NOW))
            .unwrap();
        let phase = with_token(&store, &token).unwrap().phase;
        assert_eq!(phase.0.len(), 33);
        // The placement that carries it is a tiny frame, not a 1 MiB JSON number array.
        let key = fmv_protocol::InstanceKey::new(fmv_core::AreaId { game: GameId::Ff7, key: "md1stin".into() }, phase);
        let frame = fmv_protocol::encode(&fmv_protocol::ServerMsg::InstancePlacement {
            placement: fmv_protocol::Placement {
                account: reg.account,
                key,
                party: None,
                phase_owner: reg.account,
                story_advance_allowed: true,
            },
        })
        .expect("a placement with the biggest possible progression fits a frame");
        assert!(frame.len() < 1024, "{} bytes", frame.len());
    }

    #[test]
    fn an_account_without_an_avatar_gets_one_on_login() {
        let (_dir, store) = store();
        let (account, token) = store.create_account("Cid", NOW).unwrap();
        assert!(store.avatar_for_account(account).unwrap().is_none());
        let login = login(&store, GameId::Ff7, Auth::Token(Secret::new(token)), [8, 8, 8], None, NOW).unwrap();
        assert_eq!(login.account, account);
        assert_eq!(login.avatar.tint, [8, 8, 8]);
        assert_eq!(login.avatar.display_name, "Cid");
        assert!(store.avatar_for_account(account).unwrap().is_some());
    }

    #[test]
    fn only_looks_the_game_offers_are_kept_and_they_are_canonical() {
        let ff7 = |raw: &str| clean_appearance_ref(GameId::Ff7, Some(raw));
        // Every playable id of the FF7 adapter is accepted, as the adapter spells it.
        let models = fmv_ff7::playable::playable_field_models();
        assert!(models.len() >= 4);
        for model in &models {
            assert_eq!(ff7(model.appearance_ref).as_deref(), Some(model.appearance_ref));
        }
        // Case, padding and control characters are cleaned (core's rule) and the stable id comes out.
        assert_eq!(ff7("  FF7/Cloud \n").as_deref(), Some("ff7/cloud"));
        assert_eq!(ff7("ff7/cl\u{1b}oud").as_deref(), Some("ff7/cloud"));
        // Everything else is dropped: unknown ids, other games' ids, paths, invisible characters, junk.
        for junk in [
            "",
            "   ",
            "nobody",
            "ff7/nobody",
            "fixture/cloud",
            "../../etc/passwd",
            "ff7/../cloud",
            "ff7\\cloud",
            "/ff7/cloud",
            "\u{1b}[31mff7/cloud",
            "ff7/cl\u{200B}oud",
            "ff7/cloud/../tifa",
            "ff7/cloud ff7/tifa",
        ] {
            assert_eq!(ff7(junk), None, "{junk:?}");
        }
        assert_eq!(clean_appearance_ref(GameId::Ff7, None), None);
        // A game whose adapter lists no looks accepts none, whatever the client says.
        assert_eq!(clean_appearance_ref(GameId::Fixture, Some("ff7/cloud")), None);
        assert_eq!(clean_appearance_ref(GameId::Fixture, Some("test-model")), None);
    }

    #[test]
    fn an_appearance_over_the_limit_never_burns_a_name() {
        // The audited input: 64 chars of a 4-byte character is within the 64-CHAR cap and 256 BYTES. The
        // old server accepted it and the old store refused it after the account was committed.
        let emoji = "\u{1F600}".repeat(MAX_APPEARANCE_REF);
        assert_eq!(emoji.chars().count(), 64);
        assert_eq!(clean_appearance_ref(GameId::Ff7, Some(&emoji)), None);
        // The boundaries of the audit and everything longer: none of it is a look the game offers.
        for n in [42, 43, 64, 65, 10_000] {
            assert_eq!(clean_appearance_ref(GameId::Ff7, Some(&"\u{6F22}".repeat(n))), None, "{n}");
            assert_eq!(clean_appearance_ref(GameId::Ff7, Some(&"a".repeat(n))), None, "{n}");
        }
        // The limit is core's constant, shared with the store (it counts the same characters).
        assert_eq!(MAX_APPEARANCE_REF, fmv_persist::MAX_APPEARANCE_REF_CHARS);

        // End to end on a real store, as the attack did it: the registration succeeds with the default
        // look, the name is taken by exactly that registration, and no second account appears.
        let (_dir, store) = store();
        let victim = register_ff7(&store, "Victim", [0; 3], Some(&emoji)).expect("an unusable look must not fail the registration");
        assert_eq!(victim.avatar.appearance_ref, None);
        assert!(victim.issued_token.is_some());
        assert_eq!(store.account_count(), 1);
        assert_eq!(
            register_ff7(&store, "Victim", [0; 3], None).unwrap_err(),
            LoginError::Refused(RejectCode::NameTaken, "that display name is already taken"),
        );
    }

    #[test]
    fn stored_junk_never_reaches_other_players() {
        let (_dir, store) = store();
        let (account, token) = store.create_account("Legacy", NOW).unwrap();
        let mut profile = store.create_avatar(account, [1, 2, 3], DEFAULT_ARCHETYPE, NOW).unwrap();
        // A profile written by an older build (or by hand): a path where an id belongs.
        profile.set_override(GameId::Ff7, Some("../../etc/passwd".into()));
        store.put_avatar(&profile).unwrap();
        let again = with_token(&store, &token).unwrap();
        assert_eq!(again.avatar.appearance_ref, None);
        assert_eq!(again.avatar.display_name, "Legacy");
        // A valid stored id still comes through.
        profile.set_override(GameId::Ff7, Some("ff7/barret".into()));
        store.put_avatar(&profile).unwrap();
        assert_eq!(with_token(&store, &token).unwrap().avatar.appearance_ref.as_deref(), Some("ff7/barret"));
    }

    #[test]
    fn the_rate_limit_hook_runs_only_for_accounts_that_were_created() {
        let (_dir, store) = store();
        let created = Cell::new(0);
        let go = |name: &str| {
            let registration = validate_registration(GameId::Fixture, name, None)?;
            register(&store, GameId::Fixture, registration, [0; 3], NOW, || created.set(created.get() + 1))
        };
        // A name with nothing visible is refused before anything is created.
        assert!(go("\u{200B}").is_err());
        assert_eq!(created.get(), 0);
        go("Reeve").unwrap();
        assert_eq!(created.get(), 1);
        // A taken name creates nothing either.
        assert_eq!(go("reeve").unwrap_err().code(), RejectCode::NameTaken);
        assert_eq!(created.get(), 1);
    }

    #[test]
    fn a_full_store_refuses_registrations_but_not_logins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_with(dir.path().join("state"), fmv_persist::StoreOptions::default().with_max_accounts(1)).unwrap();
        let first = register_ff7(&store, "First", [0; 3], None).unwrap();
        let err = register_ff7(&store, "Second", [0; 3], None).unwrap_err();
        assert_eq!(err.code(), RejectCode::RegistrationClosed);
        assert!(err.client_text().contains("not accepting new accounts"), "{}", err.client_text());
        // Logins are unaffected, and the refused registration took no name.
        assert!(with_token(&store, &first.issued_token.unwrap()).is_ok());
        assert_eq!(store.account_count(), 1);
    }

    #[test]
    fn validation_cleans_the_name_and_looks_without_touching_the_store() {
        let reg = validate_registration(GameId::Ff7, "  Cloud\n", Some("FF7/Cloud")).unwrap();
        assert_eq!((reg.name(), reg.appearance()), ("Cloud", Some("ff7/cloud")));
        assert_eq!(validate_registration(GameId::Ff7, "Cloud", Some("nope")).unwrap().appearance(), None);
        assert_eq!(validate_registration(GameId::Ff7, " ", None).unwrap_err().code(), RejectCode::InvalidName);
    }

    #[test]
    fn an_undelivered_registration_is_released_and_its_name_is_free_again() {
        let (_dir, store) = store();
        let ghost = register_ff7(&store, "Ghost", [0; 3], Some("ff7/tifa")).unwrap();
        let token = ghost.issued_token.clone().unwrap();
        assert_eq!(release_unclaimed(&store, ghost.account), Release::Released);
        // The name can be registered again, and the lost token no longer opens anything.
        assert_eq!(with_token(&store, &token).unwrap_err().code(), RejectCode::InvalidToken);
        let again = register_ff7(&store, "Ghost", [0; 3], None).expect("the name is free again");
        assert_ne!(again.account, ghost.account, "ids are never reused");
        // Releasing twice (or an account that never existed) is "nothing to release", not an error.
        assert_eq!(release_unclaimed(&store, ghost.account), Release::NotReleasable);
    }

    #[test]
    fn an_account_that_has_logged_in_is_never_released() {
        let (_dir, store) = store();
        let player = register_ff7(&store, "Player", [0; 3], None).unwrap();
        // Its owner holds the token and has used it: a late release (a delivery that only looked
        // lost) must not delete the account.
        with_token(&store, &player.issued_token.unwrap()).unwrap();
        assert_eq!(release_unclaimed(&store, player.account), Release::NotReleasable);
        assert!(register_ff7(&store, "Player", [0; 3], None).is_err(), "still taken");
    }

    // ---- character progress at login (M2-d) --------------------------------------------------------

    #[test]
    fn a_new_account_plays_the_character_its_look_names_and_has_earned_nothing() {
        let (_dir, store) = store();
        let tifa = register_ff7(&store, "Tifa", [0; 3], Some("ff7/tifa")).unwrap();
        assert_eq!(tifa.progress, ProgressSeed::of(2, None));
        // No look, or one the game does not offer: the default look and character.
        let plain = register_ff7(&store, "Plain", [0; 3], None).unwrap();
        assert_eq!(plain.progress, ProgressSeed::default());
        let odd = register_ff7(&store, "Odd", [0; 3], Some("not-a-look")).unwrap();
        assert_eq!(odd.progress.character, progress::DEFAULT_CHARACTER);
        // A game that has no characters of its own: everybody is the default.
        let fixture = register_for(GameId::Fixture, &store, "Fixy", [0; 3], Some("ff7/barret")).unwrap();
        assert_eq!(fixture.progress, ProgressSeed::default());
        // Nothing was written for any of them.
        for login in [&tifa, &plain, &odd] {
            assert_eq!(store.get_character_progress(login.account, GameId::Ff7).unwrap(), None);
        }
    }

    #[test]
    fn logging_in_loads_the_stored_character_progress_and_the_character_of_the_look() {
        let (_dir, store) = store();
        let reg = register_ff7(&store, "Barret", [0; 3], Some("ff7/barret")).unwrap();
        let token = reg.issued_token.unwrap();
        // Before anything was earned: the character of the look, nothing stored.
        let first = with_token(&store, &token).unwrap();
        assert_eq!(first.progress, ProgressSeed::of(1, None));

        // After a battle: what the store holds comes back, as the stored numbers.
        let record = fmv_persist::CharacterProgress { exp: 700, ap: 9, gil: 250, ..fmv_persist::CharacterProgress::new(reg.account, GameId::Ff7, 1, 8, NOW) };
        store.put_character_progress(&record).unwrap();
        let again = with_token(&store, &token).unwrap();
        assert_eq!(again.progress.character, 1);
        assert_eq!(again.progress.stored, Some(Progress { character: 1, level: 8, exp: 700, ap: 9, gil: 250, limit: 0 }));
        // It is not the story: the phase is untouched by it.
        assert_eq!(again.phase, PhaseKey::default());
        // Another game's record is not this game's.
        store.put_character_progress(&fmv_persist::CharacterProgress::new(reg.account, GameId::Ff8, 0, 40, NOW)).unwrap();
        assert_eq!(with_token(&store, &token).unwrap().progress, again.progress);
    }

    /// A character record that cannot be read fails the login (like a story blob that cannot be read
    /// does): a default would be written over it by the next battle, and an operator has to look at it
    /// first. The client is told the store failed, nothing about the file.
    #[test]
    fn a_damaged_character_record_fails_the_login_and_is_left_for_the_operator() {
        let (dir, store) = store();
        let reg = register_ff7(&store, "Cid", [0; 3], Some("ff7/cid")).unwrap();
        let token = reg.issued_token.unwrap();
        store.put_character_progress(&fmv_persist::CharacterProgress::new(reg.account, GameId::Ff7, 8, 7, NOW)).unwrap();
        let file = dir.path().join("state").join("progression").join(reg.account.0.to_string()).join("Ff7.character.json");
        std::fs::write(&file, b"{ not json").unwrap();
        let err = with_token(&store, &token).unwrap_err();
        assert!(matches!(err, LoginError::Internal(_)), "{err:?}");
        assert!(!err.client_text().contains("json") && !err.client_text().contains("state"), "{}", err.client_text());
        assert_eq!(std::fs::read(&file).unwrap(), b"{ not json", "the damaged file is still there to be looked at");
    }

    #[test]
    fn store_failures_are_internal_and_their_text_stays_local() {
        let e = LoginError::Internal("I/O error on /home/owner/state/index.log".into());
        assert!(!e.client_text().contains("owner"), "{}", e.client_text());
        assert_eq!(e.code(), RejectCode::Internal);
        assert_eq!(LoginError::Refused(RejectCode::NameTaken, "x").client_text(), "x");
        assert_eq!(LoginError::Refused(RejectCode::NameTaken, "x").code(), RejectCode::NameTaken);
        assert!(matches!(map_store_error(PersistError::IdExhausted), LoginError::Internal(_)));
    }
}
