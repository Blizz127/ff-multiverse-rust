//! Account login and registration over the persistent store (`fmv_persist::Store`).
//!
//! A connection proves who it is with `Hello::auth`: a token (log in) or a display name
//! (register). Everything a player looks like in the world comes from the STORED avatar profile,
//! never from what the client sends after registration: the client's `tint` and
//! `appearance_ref` only seed a NEW avatar.
//!
//! Every function here does blocking disk I/O. Async callers must run them under
//! `spawn_blocking` (see `conn`).
//!
//! A registered display name is unique and cannot be recovered without its token, so the token
//! is returned in [`Login::issued_token`] exactly once and the server keeps only its hash.

use fmv_core::{AccountId, AvatarAppearance, GameId, PhaseKey};
use fmv_persist::{PersistError, Store};
use fmv_protocol::Auth;

/// Base archetype of a new avatar (every adapter maps archetypes to its closest model).
pub const DEFAULT_ARCHETYPE: &str = "default";

/// Longest `appearance_ref` (in chars) the server keeps. Together with the 24-char display name
/// this bounds every entity snapshot, and therefore the size of the `AreaSnapshot` frame.
pub const MAX_APPEARANCE_REF: usize = 64;

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
}

/// Why a login failed.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    /// The client's fault. The text goes to the client in a `Reject`: it never echoes anything the
    /// client sent and never says whether a token or a name exists.
    Refused(String),
    /// The server could not read or write its store. The client gets a generic `Reject`; the
    /// cause (which may name local paths) is for the log only.
    Internal(String),
}

impl LoginError {
    /// The text for the client's `Reject`.
    pub fn client_text(&self) -> &str {
        match self {
            LoginError::Refused(text) => text,
            LoginError::Internal(_) => "the account store failed; try again later",
        }
    }
}

fn refused(text: &str) -> LoginError {
    LoginError::Refused(text.to_string())
}

fn map_store_error(e: PersistError) -> LoginError {
    match e {
        PersistError::InvalidToken => refused("invalid or unknown token"),
        PersistError::InvalidName => refused("display name must not be empty"),
        PersistError::NameTaken(_) => refused("that display name is already taken"),
        other => LoginError::Internal(other.to_string()),
    }
}

/// The appearance ref is broadcast to every player, so it is bounded and stripped of control
/// characters exactly like the display name.
pub fn clean_appearance_ref(raw: Option<String>) -> Option<String> {
    let cleaned: String = raw?.chars().filter(|c| !c.is_control()).take(MAX_APPEARANCE_REF).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Authenticate or register. `now` is unix seconds; `tint` and `appearance_ref` are the look
/// requested for a new avatar and are ignored when logging in with a token.
pub fn login(
    store: &Store,
    game: GameId,
    auth: Auth,
    tint: [u8; 3],
    appearance_ref: Option<String>,
    now: u64,
) -> Result<Login, LoginError> {
    let (account, profile, issued_token) = match auth {
        Auth::Token(token) => {
            let account = store.authenticate(token.expose()).map_err(map_store_error)?;
            let profile = match store.avatar_for_account(account).map_err(map_store_error)? {
                Some(profile) => profile,
                // An account without an avatar (made by other tooling): give it one now.
                None => store.create_avatar(account, tint, DEFAULT_ARCHETYPE, now).map_err(map_store_error)?,
            };
            (account, profile, None)
        }
        Auth::Register { display_name } => {
            let (account, token) = store.create_account(&display_name, now).map_err(map_store_error)?;
            let mut profile = store.create_avatar(account, tint, DEFAULT_ARCHETYPE, now).map_err(map_store_error)?;
            if let Some(appearance) = clean_appearance_ref(appearance_ref) {
                profile.set_override(game, Some(appearance));
                profile.updated_at = now;
                store.put_avatar(&profile).map_err(map_store_error)?;
            }
            (account, profile, Some(token))
        }
    };
    let progression = store.get_progression(account, game).map_err(map_store_error)?;
    Ok(Login {
        account,
        avatar: profile.appearance_for(game),
        phase: PhaseKey(progression.blob),
        issued_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_persist::Progression;
    use fmv_protocol::Secret;

    const NOW: u64 = 1_000;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        (dir, store)
    }

    fn register(store: &Store, name: &str, tint: [u8; 3], appearance: Option<&str>) -> Result<Login, LoginError> {
        login(store, GameId::Ff7, Auth::Register { display_name: name.into() }, tint, appearance.map(str::to_string), NOW)
    }

    fn with_token(store: &Store, token: &str) -> Result<Login, LoginError> {
        login(store, GameId::Ff7, Auth::Token(Secret::new(token)), [9, 9, 9], Some("ignored".into()), NOW + 5)
    }

    #[test]
    fn registering_creates_the_account_and_avatar_and_issues_the_token_once() {
        let (_dir, store) = store();
        let reg = register(&store, "  Cloud\n", [10, 20, 30], Some("cloud_field_model")).unwrap();
        assert_eq!(reg.avatar.display_name, "Cloud", "the name is cleaned like everywhere else");
        assert_eq!(reg.avatar.tint, [10, 20, 30]);
        assert_eq!(reg.avatar.appearance_ref.as_deref(), Some("cloud_field_model"));
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
        let reg = register(&store, "Tifa", [1, 2, 3], Some("tifa_model")).unwrap();
        let token = reg.issued_token.unwrap();
        // The tint and appearance sent with a token login are ignored: the stored profile wins.
        let again = with_token(&store, &token).unwrap();
        assert_eq!(again.avatar.tint, [1, 2, 3]);
        assert_eq!(again.avatar.appearance_ref.as_deref(), Some("tifa_model"));
        assert_eq!(again.avatar.display_name, "Tifa");
    }

    #[test]
    fn the_appearance_override_is_per_game() {
        let (_dir, store) = store();
        let reg = register(&store, "Aerith", [4, 5, 6], Some("aerith_ff7")).unwrap();
        let token = reg.issued_token.unwrap();
        // The same account hosted by a different game has no override there.
        let other = login(&store, GameId::Fixture, Auth::Token(Secret::new(token)), [0; 3], None, NOW).unwrap();
        assert_eq!(other.avatar.appearance_ref, None);
        assert_eq!(other.avatar.display_name, "Aerith");
    }

    #[test]
    fn refusals_say_nothing_about_which_tokens_or_names_exist() {
        let (_dir, store) = store();
        register(&store, "Barret", [0; 3], None).unwrap();
        // Name already taken (case-insensitively), empty, or only invisible characters.
        for taken in ["Barret", "BARRET", "  barret "] {
            assert_eq!(register(&store, taken, [0; 3], None).unwrap_err(), LoginError::Refused("that display name is already taken".into()));
        }
        for empty in ["", "   ", "\u{200B}\u{FEFF}", "\u{7}"] {
            assert_eq!(register(&store, empty, [0; 3], None).unwrap_err(), LoginError::Refused("display name must not be empty".into()), "{empty:?}");
        }
        // Unknown, malformed and oversized tokens are one and the same refusal.
        for bad in ["", "nope", "fmvt_0000", &"x".repeat(10_000)] {
            assert_eq!(with_token(&store, bad).unwrap_err(), LoginError::Refused("invalid or unknown token".into()));
        }
        // Refused registrations did not burn the name or create accounts.
        assert!(register(&store, "Yuffie", [0; 3], None).is_ok());
    }

    #[test]
    fn the_phase_comes_from_the_accounts_progression() {
        let (_dir, store) = store();
        let reg = register(&store, "Vincent", [0; 3], None).unwrap();
        let token = reg.issued_token.unwrap();
        store.put_progression(&Progression::new(reg.account, GameId::Ff7, vec![7, 7], NOW)).unwrap();
        assert_eq!(with_token(&store, &token).unwrap().phase, PhaseKey(vec![7, 7]));
        // Another game's progression is not this game's phase.
        store.put_progression(&Progression::new(reg.account, GameId::Fixture, vec![1], NOW)).unwrap();
        assert_eq!(with_token(&store, &token).unwrap().phase, PhaseKey(vec![7, 7]));
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
    fn appearance_refs_are_bounded_and_cleaned() {
        let long = "m".repeat(10_000);
        assert_eq!(clean_appearance_ref(Some(format!("\u{1b}[31m{long}"))).unwrap().chars().count(), MAX_APPEARANCE_REF);
        assert_eq!(clean_appearance_ref(Some(" \n".into())), None);
        assert_eq!(clean_appearance_ref(None), None);
        assert_eq!(clean_appearance_ref(Some(" model ".into())).as_deref(), Some("model"));
        // Registration stores the cleaned, bounded ref.
        let (_dir, store) = store();
        let reg = register(&store, "Reeve", [0; 3], Some(&long)).unwrap();
        assert_eq!(reg.avatar.appearance_ref.unwrap().chars().count(), MAX_APPEARANCE_REF);
    }

    #[test]
    fn store_failures_are_internal_and_their_text_stays_local() {
        let e = LoginError::Internal("I/O error on /home/owner/state/index.json".into());
        assert!(!e.client_text().contains("owner"), "{}", e.client_text());
        assert_eq!(LoginError::Refused("x".into()).client_text(), "x");
        assert!(matches!(map_store_error(PersistError::IdExhausted), LoginError::Internal(_)));
    }
}
