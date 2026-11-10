use super::*;
use crate::saver::{persist, SaveJob};
use fmv_persist::Progression;
use fmv_protocol::{Auth, BattleReward, BattleRules, Secret, SyntheticRules};
use std::cell::Cell;
use std::sync::Arc;

const NOW: u64 = 1_700_000_000;

/// A request for a new character.
fn request(tint: [u8; 3], body: Option<&str>, slot: Option<u8>) -> NewCharacter {
    NewCharacter { tint, body: body.map(str::to_owned), slot }
}

/// Authenticate or register in one call. (`conn` does it in steps, so that the registration rate
/// limits sit between validating and creating.) `character` is what a registration asks for and is
/// ignored when logging in with a token.
fn login(store: &Store, game: GameId, auth: Auth, character: NewCharacter, now: u64) -> Result<Login, LoginError> {
    match auth {
        Auth::Token(token) => login_with_token(store, game, &token, character.tint, now),
        Auth::Register { display_name } => {
            let registration = validate_registration(game, &display_name, &character)?;
            register(store, game, registration, now, || {})
        }
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state")).unwrap();
    (dir, store)
}

fn register_for(game: GameId, store: &Store, name: &str, character: NewCharacter) -> Result<Login, LoginError> {
    login(store, game, Auth::Register { display_name: name.into() }, character, NOW)
}

/// An FF7 registration with a body and a slot.
fn register_ff7(store: &Store, name: &str, tint: [u8; 3], body: Option<&str>, slot: Option<u8>) -> Result<Login, LoginError> {
    register_for(GameId::Ff7, store, name, request(tint, body, slot))
}

/// An FF7 registration that chose nothing (what every headless bot does).
fn plain_ff7(store: &Store, name: &str) -> Result<Login, LoginError> {
    register_ff7(store, name, [0; 3], None, None)
}

fn with_token(store: &Store, token: &str) -> Result<Login, LoginError> {
    login(store, GameId::Ff7, Auth::Token(Secret::new(token)), request([9, 9, 9], Some("slight"), Some(7)), NOW + 5)
}

fn character_file(dir: &tempfile::TempDir, account: AccountId) -> std::path::PathBuf {
    dir.path().join("state").join("characters").join(format!("{}.json", account.0))
}

fn progress_file(dir: &tempfile::TempDir, account: AccountId) -> std::path::PathBuf {
    dir.path().join("state").join("progression").join(account.0.to_string()).join("Ff7.character.json")
}

// ---- registration and login -------------------------------------------------------------------------

#[test]
fn registering_creates_the_account_avatar_and_character_and_issues_the_token_once() {
    let (_dir, store) = store();
    let reg = register_ff7(&store, "  Cloud\n", [10, 20, 30], Some("slight"), Some(2)).unwrap();
    assert_eq!(reg.avatar.display_name, "Cloud", "the name is cleaned like everywhere else");
    assert_eq!(reg.avatar.tint, [10, 20, 30]);
    assert_eq!(reg.avatar.appearance_ref.as_deref(), Some("ff7/body/slight"), "the look is the binding: a generic body");
    assert!(reg.progression.is_empty(), "a new account has no story progression");
    let token = reg.issued_token.clone().expect("a registration issues a token");
    assert!(!token.is_empty());
    // What the player is told about its own character.
    let card = &reg.card;
    assert_eq!((card.name.as_str(), card.tint, card.body.as_deref()), ("Cloud", [10, 20, 30], Some("slight")));
    assert_eq!((card.binding.as_deref(), card.slot, card.migrated), (Some("ff7/body/slight"), Some(2), false));
    assert_eq!(reg.progress, ProgressSeed::of(2, None), "it fights as the slot it chose");

    // The token logs the same account in again, and issues nothing the second time.
    let again = with_token(&store, &token).unwrap();
    assert_eq!(again.account, reg.account);
    assert!(again.issued_token.is_none());
    assert_eq!(again.avatar, reg.avatar);
    assert_eq!(again.card, reg.card);
    assert_eq!(again.progress, reg.progress);
}

#[test]
fn after_registration_the_client_cannot_choose_its_character() {
    let (_dir, store) = store();
    let reg = register_ff7(&store, "Tifa", [1, 2, 3], Some("sturdy"), Some(2)).unwrap();
    let token = reg.issued_token.unwrap();
    // The colour, body and slot sent with a token login are ignored: the stored character decides.
    let again = with_token(&store, &token).unwrap();
    assert_eq!(again.avatar.tint, [1, 2, 3]);
    assert_eq!(again.avatar.appearance_ref.as_deref(), Some("ff7/body/sturdy"));
    assert_eq!(again.avatar.display_name, "Tifa");
    assert_eq!((again.card.slot, again.card.body.as_deref()), (Some(2), Some("sturdy")));
    assert_eq!(again.progress.character, 2);
}

#[test]
fn the_binding_and_the_slot_are_per_game() {
    let (_dir, store) = store();
    let reg = register_ff7(&store, "Aerith", [4, 5, 6], Some("slight"), Some(3)).unwrap();
    let token = reg.issued_token.unwrap();
    // The same account hosted by a different game has no entry there: no look, no slot, and (a game
    // that has no combat slots) the default character for its progress. Entering it adds the game's
    // entry and changes nothing of Ff7's.
    let other = login(&store, GameId::Fixture, Auth::Token(Secret::new(token.clone())), request([0; 3], None, None), NOW).unwrap();
    assert_eq!(other.avatar.appearance_ref, None);
    assert_eq!(other.avatar.display_name, "Aerith");
    assert_eq!((other.card.slot, other.card.binding), (None, None));
    assert_eq!(other.progress.character, progress::DEFAULT_CHARACTER);
    let back = with_token(&store, &token).unwrap();
    assert_eq!((back.card.slot, back.card.binding.as_deref()), (Some(3), Some("ff7/body/slight")));
    let record = store.get_player_character(reg.account).unwrap().unwrap();
    assert_eq!(record.games.len(), 2);
}

#[test]
fn refusals_are_typed_and_say_nothing_about_which_tokens_or_names_exist() {
    let (_dir, store) = store();
    plain_ff7(&store, "Barret").unwrap();
    // Name already taken (however it is written).
    for taken in ["Barret", "BARRET", "  barret ", "B\u{200B}arret"] {
        assert_eq!(
            plain_ff7(&store, taken).unwrap_err(),
            LoginError::Refused(RejectCode::NameTaken, "that display name is already taken"),
            "{taken:?}"
        );
    }
    // Nothing visible left.
    for empty in ["", "   ", "\u{200B}\u{FEFF}", "\u{7}"] {
        assert_eq!(
            plain_ff7(&store, empty).unwrap_err(),
            LoginError::Refused(RejectCode::InvalidName, "display name must not be empty"),
            "{empty:?}"
        );
    }
    // Unknown, malformed and oversized tokens are one and the same refusal.
    for bad in ["", "nope", "fmvt_0000", &"x".repeat(10_000)] {
        assert_eq!(with_token(&store, bad).unwrap_err(), LoginError::Refused(RejectCode::InvalidToken, "invalid or unknown token"));
    }
    // Refused registrations did not burn the name or create accounts.
    assert!(plain_ff7(&store, "Yuffie").is_ok());
}

#[test]
fn a_look_alike_name_is_refused_with_its_own_reason() {
    let (_dir, store) = store();
    // "Tifa" with a Cyrillic T: the classic impersonation.
    let err = plain_ff7(&store, "\u{0422}ifa").unwrap_err();
    assert_eq!(err.code(), RejectCode::InvalidName);
    assert!(err.client_text().contains("writing systems"), "{}", err.client_text());
    // Another script entirely is a different reason, same code.
    assert_eq!(validate_registration(GameId::Ff7, "\u{0422}ifa", &request([0; 3], None, None)).unwrap_err(), err);
    // And the whole thing happened before any account existed.
    assert!(plain_ff7(&store, "Tifa").is_ok());
}

#[test]
fn the_progression_comes_from_the_accounts_record_for_this_game() {
    let (_dir, store) = store();
    let reg = plain_ff7(&store, "Vincent").unwrap();
    let token = reg.issued_token.unwrap();
    assert!(with_token(&store, &token).unwrap().progression.is_empty(), "never stored: a new account");
    store.put_progression(&Progression::new(reg.account, GameId::Ff7, vec![7, 7], NOW)).unwrap();
    assert_eq!(with_token(&store, &token).unwrap().progression, vec![7, 7]);
    // Another game's progression is not this game's.
    store.put_progression(&Progression::new(reg.account, GameId::Fixture, vec![1], NOW)).unwrap();
    assert_eq!(with_token(&store, &token).unwrap().progression, vec![7, 7]);
    // Whatever the adapter does not read keeps the old identity: one phase per distinct blob.
    assert_eq!(phase_key(with_token(&store, &token).unwrap().progression), PhaseKey(vec![7, 7]));
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
    let reg = plain_ff7(&store, "Hugo").unwrap();
    let token = reg.issued_token.unwrap();
    store
        .put_progression(&Progression::new(reg.account, GameId::Ff7, vec![0xAB; fmv_persist::MAX_BLOB_BYTES], NOW))
        .unwrap();
    let kept = with_token(&store, &token).unwrap().progression;
    assert_eq!(kept.len(), 33, "kept as its digest, not as 256 KiB per player");
    assert_eq!(kept[0], PHASE_DIGEST_TAG);
    let phase = phase_key(kept);
    assert_eq!(phase.0.len(), 33);
    // The placement that carries it is a tiny frame, not a 1 MiB JSON number array.
    let key = fmv_protocol::InstanceKey::new(fmv_core::AreaId { game: GameId::Ff7, key: "md1stin".into() }, phase);
    let frame = fmv_protocol::encode(&fmv_protocol::ServerMsg::InstancePlacement {
        placement: fmv_protocol::Placement { account: reg.account, key, party: None, phase_owner: reg.account, story_advance_allowed: true },
    })
    .expect("a placement with the biggest possible progression fits a frame");
    assert!(frame.len() < 1024, "{} bytes", frame.len());
}

#[test]
fn an_account_without_an_avatar_gets_one_on_login_and_a_character() {
    let (_dir, store) = store();
    let (account, token) = store.create_account("Cid", NOW).unwrap();
    assert!(store.avatar_for_account(account).unwrap().is_none());
    let login = login(&store, GameId::Ff7, Auth::Token(Secret::new(token)), request([8, 8, 8], None, None), NOW).unwrap();
    assert_eq!(login.account, account);
    assert_eq!(login.avatar.tint, [8, 8, 8]);
    assert_eq!(login.avatar.display_name, "Cid");
    assert!(store.avatar_for_account(account).unwrap().is_some());
    assert!(login.card.migrated, "it predates the record");
    assert_eq!(login.card.slot, Some(0), "no look: what the builds before fought it as");
}

// ---- what a new character may be --------------------------------------------------------------------

#[test]
fn a_new_character_is_created_with_a_record_a_card_and_a_mirror_for_a_rolled_back_server() {
    let (dir, store) = store();
    let reg = register_ff7(&store, "Wedge", [200, 10, 10], Some("sturdy"), Some(1)).unwrap();
    let record = store.get_player_character(reg.account).unwrap().expect("the registration wrote the record");
    assert_eq!(record.origin, fmv_persist::CharacterOrigin::Created);
    assert_eq!((record.identity.name.as_str(), record.identity.tint, record.identity.body.as_str()), ("Wedge", [200, 10, 10], "sturdy"));
    assert_eq!((record.slot(GameId::Ff7), record.binding(GameId::Ff7)), (Some(1), Some("ff7/body/sturdy")));
    assert_eq!(
        record.game(GameId::Ff7).unwrap().progress,
        vec![
            fmv_persist::ProgressRef::Character { game: GameId::Ff7, character: 1 },
            fmv_persist::ProgressRef::Story { game: GameId::Ff7 }
        ]
    );
    assert!(character_file(&dir, reg.account).is_file());
    // The avatar profile keeps the copy a v7 build reads, with the canon look of the SLOT: a server rolled back to
    // v7 (which fights an account as the character its look names) still fights the character as Barret. v8
    // itself never reads it.
    let avatar = store.avatar_for_account(reg.account).unwrap().unwrap();
    assert_eq!(avatar.override_for(GameId::Ff7), Some("ff7/barret"));
    assert_eq!(progress::character_for_look(GameId::Ff7, avatar.override_for(GameId::Ff7)), 1);
    assert_eq!((avatar.display_name.as_str(), avatar.tint), ("Wedge", [200, 10, 10]));
    // ...and what other players see is the binding, not that mirror.
    assert_eq!(reg.avatar.appearance_ref.as_deref(), Some("ff7/body/sturdy"));
}

#[test]
fn cloud_is_refused_to_a_new_character_and_a_refusal_creates_nothing() {
    let (_dir, store) = store();
    for slot in [0u8, 9, 200] {
        let err = register_ff7(&store, "Cloudy", [1, 2, 3], None, Some(slot)).unwrap_err();
        assert_eq!(err.code(), RejectCode::BadRequest, "slot {slot}");
        assert!(err.client_text().contains("combat slot"), "{}", err.client_text());
    }
    assert_eq!(store.account_count(), 0, "nothing was created");
    // The name is free, and the default slot is Barret's.
    let plain = register_ff7(&store, "Cloudy", [1, 2, 3], None, None).unwrap();
    assert_eq!((plain.card.slot, plain.progress.character), (Some(1), 1));
    // Two players may hold the same slot, and each is told its own name.
    let twin = register_ff7(&store, "Twin", [3, 2, 1], None, Some(1)).unwrap();
    assert_eq!(twin.card.slot, Some(1));
    assert_ne!(twin.card.name, plain.card.name);
}

#[test]
fn a_game_without_combat_slots_keeps_no_slot_and_ignores_what_the_client_asked() {
    let (_dir, store) = store();
    let fixture = register_for(GameId::Fixture, &store, "Fixy", request([0; 3], Some("slight"), Some(0))).unwrap();
    assert_eq!((fixture.card.slot, fixture.card.binding.clone(), fixture.card.body.clone()), (None, None, None));
    assert_eq!(fixture.progress, ProgressSeed::default());
    assert_eq!(fixture.avatar.appearance_ref, None);
    let record = store.get_player_character(fixture.account).unwrap().unwrap();
    assert_eq!(record.slot(GameId::Fixture), None);
    assert!(record.game(GameId::Fixture).unwrap().has_story_ref());
    // Nothing was written for progress.
    assert_eq!(store.get_character_progress(fixture.account, GameId::Fixture).unwrap(), None);
}

#[test]
fn an_unusable_body_never_burns_a_name() {
    // The audited input of the look before it: 64 chars of a 4-byte character is within the 64-CHAR cap and 256
    // BYTES. A body is only ever a table id: anything else is the default body, and the registration goes through.
    let emoji = "\u{1F600}".repeat(MAX_APPEARANCE_REF);
    assert_eq!(emoji.chars().count(), 64);
    let (_dir, store) = store();
    let default = fmv_ff7::playable::body_or_default(None);
    for body in [emoji.as_str(), "", "nobody", "../../etc/passwd", &"x".repeat(10_000), "\u{1b}[31mslight"] {
        let name = format!("V{}", store.account_count());
        let reg = register_ff7(&store, &name, [0; 3], Some(body), None).unwrap_or_else(|e| panic!("{body:?}: {e:?}"));
        assert_eq!(reg.card.body.as_deref(), Some(default.archetype), "{body:?}");
        assert_eq!(reg.avatar.appearance_ref.as_deref(), Some(default.model.appearance_ref), "{body:?}");
        assert!(reg.issued_token.is_some());
    }
    assert_eq!(MAX_APPEARANCE_REF, fmv_persist::MAX_APPEARANCE_REF_CHARS);
    // A name taken by exactly that registration, and no second account appears.
    let before = store.account_count();
    assert_eq!(
        plain_ff7(&store, "V0").unwrap_err(),
        LoginError::Refused(RejectCode::NameTaken, "that display name is already taken"),
    );
    assert_eq!(store.account_count(), before);
}

#[test]
fn validation_cleans_the_name_and_checks_the_choices_without_touching_the_store() {
    let reg = validate_registration(GameId::Ff7, "  Cloud\n", &request([4, 5, 6], Some("SLIGHT"), Some(4))).unwrap();
    assert_eq!((reg.name(), reg.tint()), ("Cloud", [4, 5, 6]));
    assert_eq!((reg.creation().body.as_str(), reg.creation().entry.slot), ("slight", Some(4)));
    assert_eq!(reg.creation().entry.binding.as_deref(), Some("ff7/body/slight"));
    assert_eq!(validate_registration(GameId::Ff7, " ", &request([0; 3], None, None)).unwrap_err().code(), RejectCode::InvalidName);
    assert_eq!(validate_registration(GameId::Ff7, "Ok", &request([0; 3], None, Some(0))).unwrap_err().code(), RejectCode::BadRequest);
}

#[test]
fn the_rate_limit_hook_runs_only_for_accounts_that_were_created() {
    let (_dir, store) = store();
    let created = Cell::new(0);
    let go = |name: &str, slot: Option<u8>| {
        let registration = validate_registration(GameId::Ff7, name, &request([0; 3], None, slot))?;
        register(&store, GameId::Ff7, registration, NOW, || created.set(created.get() + 1))
    };
    // A name with nothing visible, and a slot nobody may take, are refused before anything is created.
    assert!(go("\u{200B}", None).is_err());
    assert!(go("Reeve", Some(0)).is_err());
    assert_eq!(created.get(), 0);
    go("Reeve", None).unwrap();
    assert_eq!(created.get(), 1);
    // A taken name creates nothing either.
    assert_eq!(go("reeve", None).unwrap_err().code(), RejectCode::NameTaken);
    assert_eq!(created.get(), 1);
}

#[test]
fn a_full_store_refuses_registrations_but_not_logins() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_with(dir.path().join("state"), fmv_persist::StoreOptions::default().with_max_accounts(1)).unwrap();
    let first = plain_ff7(&store, "First").unwrap();
    let err = plain_ff7(&store, "Second").unwrap_err();
    assert_eq!(err.code(), RejectCode::RegistrationClosed);
    assert!(err.client_text().contains("not accepting new accounts"), "{}", err.client_text());
    // Logins are unaffected, and the refused registration took no name.
    assert!(with_token(&store, &first.issued_token.unwrap()).is_ok());
    assert_eq!(store.account_count(), 1);
}

#[test]
fn an_undelivered_registration_is_released_with_its_character_and_its_name_is_free_again() {
    let (dir, store) = store();
    let ghost = register_ff7(&store, "Ghost", [0; 3], Some("slight"), Some(2)).unwrap();
    let token = ghost.issued_token.clone().unwrap();
    assert!(character_file(&dir, ghost.account).is_file());
    assert_eq!(release_unclaimed(&store, ghost.account), Release::Released);
    assert!(!character_file(&dir, ghost.account).exists(), "the character record goes with the account");
    // The name can be registered again, and the lost token no longer opens anything.
    assert_eq!(with_token(&store, &token).unwrap_err().code(), RejectCode::InvalidToken);
    let again = plain_ff7(&store, "Ghost").expect("the name is free again");
    assert_ne!(again.account, ghost.account, "ids are never reused");
    // Releasing twice (or an account that never existed) is "nothing to release", not an error.
    assert_eq!(release_unclaimed(&store, ghost.account), Release::NotReleasable);
}

#[test]
fn an_account_that_has_logged_in_is_never_released() {
    let (_dir, store) = store();
    let player = plain_ff7(&store, "Player").unwrap();
    // Its owner holds the token and has used it: a late release (a delivery that only looked
    // lost) must not delete the account.
    with_token(&store, &player.issued_token.unwrap()).unwrap();
    assert_eq!(release_unclaimed(&store, player.account), Release::NotReleasable);
    assert!(plain_ff7(&store, "Player").is_err(), "still taken");
}

// ---- character progress at login ---------------------------------------------------------------------

/// Synthetic authored initialization, deliberately distinct from the experience curve's floor.
struct StartingRules { gil: u32 }
impl BattleRules for StartingRules {
    fn build(&self, setup: &fmv_protocol::BattleSetup) -> Result<fmv_ff7_battle::rules::BattleState, fmv_protocol::BattleBuildError> {
        SyntheticRules::default().build(setup)
    }
    fn data_fingerprint(&self) -> fmv_core::Fingerprint { SyntheticRules::default().data_fingerprint() }
    fn formation(&self, id: u16) -> Option<fmv_protocol::FormationInfo> { SyntheticRules::default().formation(id) }
    fn starting_level(&self, _character: u8) -> u8 { 5 }
    fn default_formation(&self) -> Option<u16> { Some(0) }
    fn starting_exp(&self, _character: u8, _level: u8) -> u32 { 53 }
    fn starting_gil(&self) -> u32 { self.gil }
    fn starting_inventory(&self) -> Vec<fmv_core::InventoryEntry> {
        vec![fmv_core::InventoryEntry::new(3, 1), fmv_core::InventoryEntry::new(0, 3)]
    }
}

fn register_with_starting_resources(store: &Store, name: &str) -> Login {
    let registration = validate_registration(GameId::Ff7, name, &request([0; 3], None, Some(2))).unwrap();
    register_with_rules(store, GameId::Ff7, registration, NOW, || {}, &StartingRules { gil: 240 }).unwrap()
}

fn token_with_starting_rules(store: &Store, token: &str, now: u64) -> Result<Login, LoginError> {
    login_with_token_and_rules(store, GameId::Ff7, &Secret::new(token), [0; 3], now, &StartingRules { gil: 240 })
}

#[test]
fn fresh_story_and_resources_are_committed_before_the_token_and_reopen_preserves_both() {
    let (dir, store) = store();
    let story = fmv_ff7::story::StoryState::fresh_first_control().encode();
    let registration = validate_registration(GameId::Ff7, "Fresh story", &request([0; 3], None, Some(2))).unwrap();
    let created = std::cell::Cell::new(false);
    let registered = register_with_initial_state(&store, GameId::Ff7, registration, NOW, || created.set(true),
        Some(&StartingRules { gil: 240 }), Some(story.clone())).unwrap();
    assert!(created.get());
    assert_eq!(registered.progression, story);
    let account = registered.account;
    let pair = store.get_scene_state(account, GameId::Ff7).unwrap();
    assert_eq!(pair.pair.story.blob, story);
    assert_eq!(pair.pair.character.as_ref().map(Progress::from_record), registered.progress.stored);
    assert_eq!(pair.pair.character.as_ref().and_then(|record| record.inventory.clone()), registered.progress.inventory);
    let token = registered.issued_token.unwrap();
    let story_file = dir.path().join("state/progression").join(account.0.to_string()).join("Ff7.json");
    let story_bytes = std::fs::read(&story_file).unwrap();
    let resource_bytes = std::fs::read(progress_file(&dir, account)).unwrap();
    drop(store);
    let store = Store::open(dir.path().join("state")).unwrap();
    let login = token_with_starting_rules(&store, &token, NOW + 100).unwrap();
    assert_eq!(login.progression, story);
    assert_eq!(login.progress, registered.progress);
    assert_eq!(std::fs::read(story_file).unwrap(), story_bytes);
    assert_eq!(std::fs::read(progress_file(&dir, account)).unwrap(), resource_bytes);
}

#[test]
fn a_fresh_story_can_exist_without_battle_rules_and_undelivered_registration_releases_it() {
    let (_dir, store) = store();
    let story = fmv_ff7::story::StoryState::fresh_first_control().encode();
    let registration = validate_registration(GameId::Ff7, "Undelivered story", &request([0; 3], None, None)).unwrap();
    let registered = register_with_initial_state(&store, GameId::Ff7, registration, NOW, || {}, None, Some(story.clone())).unwrap();
    assert_eq!(registered.progression, story);
    assert!(registered.progress.stored.is_none());
    assert!(store.get_character_progress(registered.account, GameId::Ff7).unwrap().is_none());
    assert_eq!(release_unclaimed(&store, registered.account), Release::Released);
    assert!(plain_ff7(&store, "Undelivered story").is_ok(), "the unclaimed seed does not strand its name");
}

#[test]
fn an_initial_story_that_cannot_be_sent_is_refused_before_taking_the_name() {
    let (_dir, store) = store();
    let registration = validate_registration(GameId::Ff7, "Oversize story", &request([0; 3], None, None)).unwrap();
    let created = std::cell::Cell::new(false);
    let result = register_with_initial_state(&store, GameId::Ff7, registration, NOW, || created.set(true),
        Some(&StartingRules { gil: 240 }), Some(vec![0; MAX_PROGRESSION_BYTES + 1]));
    assert!(matches!(result, Err(LoginError::Internal(_))));
    assert!(!created.get());
    assert!(plain_ff7(&store, "Oversize story").is_ok());
}

#[test]
fn new_registration_commits_authored_starting_resources_once_and_login_never_recredits_them() {
    let (dir, store) = store();
    let registered = register_with_starting_resources(&store, "New resources");
    let initial = registered.progress.stored.unwrap();
    assert_eq!(initial, Progress { character: 2, level: 5, exp: 53, ap: 0, gil: 240, limit: 0 });
    assert_eq!(registered.progress.inventory, entries(&[(0, 3), (3, 1)]));
    let stored = store.get_character_progress(registered.account, GameId::Ff7).unwrap().unwrap();
    assert_eq!(Progress::from_record(&stored), initial);
    assert_eq!(stored.inventory, registered.progress.inventory);
    assert_eq!(stored.updated_at, NOW);
    let token = registered.issued_token.unwrap();
    // Spending the entire balance and inventory is distinct from never having a record.
    store.update_character_progress(registered.account, GameId::Ff7, |old| {
        let mut next = old.unwrap(); next.gil = 0; next.inventory = Some(vec![]); next.updated_at = NOW + 1; next
    }).unwrap();
    let file = progress_file(&dir, registered.account);
    let before = std::fs::read(&file).unwrap();
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    let login = login_with_token_and_rules(&reopened, GameId::Ff7, &Secret::new(token), [0; 3], NOW + 2, &StartingRules { gil: 999 }).unwrap();
    assert_eq!(login.progress.stored.unwrap().gil, 0);
    assert_eq!(login.progress.inventory, Some(vec![]));
    assert_eq!(std::fs::read(file).unwrap(), before, "login does not rewrite even an exhausted record");
}

#[test]
fn a_legacy_account_without_a_record_materializes_its_zero_gil_defaults_without_changing_story() {
    let (dir, store) = store();
    let registered = plain_ff7(&store, "Legacy resources").unwrap();
    assert!(store.get_character_progress(registered.account, GameId::Ff7).unwrap().is_none());
    let token = registered.issued_token.unwrap();
    let story = Progression::new(registered.account, GameId::Ff7, vec![1, 9], NOW);
    store.put_progression(&story).unwrap();
    let login = token_with_starting_rules(&store, &token, NOW + 1).unwrap();
    assert_eq!(login.progress.stored, Some(Progress { character: 1, level: 5, exp: 53, ap: 0, gil: 0, limit: 0 }));
    assert_eq!(login.progress.inventory, entries(&[(0, 3), (3, 1)]));
    assert_eq!(store.get_progression(registered.account, GameId::Ff7).unwrap(), story);
    let before = std::fs::read(progress_file(&dir, registered.account)).unwrap();
    let again = token_with_starting_rules(&store, &token, NOW + 100).unwrap();
    assert_eq!(again.progress, login.progress);
    assert_eq!(std::fs::read(progress_file(&dir, registered.account)).unwrap(), before, "the default is inserted only once");
}

#[test]
fn initialized_new_and_legacy_characters_can_commit_an_economic_scene_before_any_reward() {
    for newly_registered in [false, true] {
        let (_dir, store) = store();
        let login = if newly_registered {
            register_with_starting_resources(&store, "Before rewards")
        } else {
            let registered = plain_ff7(&store, "Before rewards").unwrap();
            token_with_starting_rules(&store, &registered.issued_token.unwrap(), NOW + 1).unwrap()
        };
        let expected_gil: u32 = if newly_registered { 240 } else { 0 };
        let next_gil = expected_gil.saturating_sub(1);
        let job = crate::saver::SceneSaveJob { account: login.account, scene_key: "test/first-economic-scene".into(),
            expected_story: vec![], next_story: vec![5], expected_gil, next_gil };
        let saved = crate::saver::persist_scene(&store, GameId::Ff7, &job, NOW + 2).expect("an authoritative record exists before any reward");
        let character = saved.pair.character.unwrap();
        assert_eq!(character.gil, next_gil);
        assert_eq!((character.level, character.exp, character.ap), (5, 53, 0));
        assert_eq!(character.inventory, entries(&[(0, 3), (3, 1)]));
        assert_eq!(saved.pair.story.blob, [5]);
    }
}

#[test]
fn resource_aware_login_preserves_existing_records_and_their_inventory_semantics_exactly() {
    for inventory in [None, Some(vec![]), entries(&[(7, 3)])] {
        let (dir, store) = store();
        let registered = plain_ff7(&store, "Existing resources").unwrap();
        let token = registered.issued_token.unwrap();
        // Older builds sometimes stored the look's slot: it remains untouched on disk.
        let mut original = CharacterProgress::new(registered.account, GameId::Ff7, 0, 7, NOW);
        original.exp = 88; original.gil = 4321; original.ap = 11; original.limit = 37; original.inventory = inventory.clone();
        store.put_character_progress(&original).unwrap();
        let file = progress_file(&dir, registered.account);
        let before = std::fs::read(&file).unwrap();
        let login = token_with_starting_rules(&store, &token, NOW + 1).unwrap();
        assert_eq!(login.progress.character, 1, "the character's fixed slot still controls the mirror");
        assert_eq!(login.progress.stored, Some(Progress::from_record(&original)));
        assert_eq!(login.progress.inventory, inventory);
        assert_eq!(std::fs::read(file).unwrap(), before);
    }
}

#[test]
fn resource_aware_login_refuses_corrupt_progress_instead_of_inserting_defaults() {
    let (dir, store) = store();
    let registered = register_with_starting_resources(&store, "Corrupt resources");
    let file = progress_file(&dir, registered.account);
    std::fs::write(&file, b"{ broken progress").unwrap();
    let error = token_with_starting_rules(&store, &registered.issued_token.unwrap(), NOW + 1).unwrap_err();
    assert!(matches!(error, LoginError::Internal(_)));
    assert_eq!(std::fs::read(file).unwrap(), b"{ broken progress");
}

#[test]
fn undelivered_registration_with_untouched_starting_resources_can_still_release_its_name() {
    let (dir, store) = store();
    let registered = register_with_starting_resources(&store, "Undelivered resources");
    assert_eq!(release_unclaimed(&store, registered.account), Release::Released);
    assert!(!progress_file(&dir, registered.account).exists());
    assert!(matches!(store.get_character_progress(registered.account, GameId::Ff7), Err(PersistError::UnknownAccount(_))));
    let replacement = register_with_starting_resources(&store, "Undelivered resources");
    assert_ne!(replacement.account, registered.account);
    assert_eq!(replacement.progress.stored.unwrap().gil, 240);
    claim_delivered(&store, replacement.account).unwrap();
    assert_eq!(release_unclaimed(&store, replacement.account), Release::NotReleasable);
}

#[test]
fn concurrent_legacy_logins_and_a_reward_cannot_replace_earned_progress_with_defaults() {
    let (_dir, store) = store();
    let registered = plain_ff7(&store, "Racing resources").unwrap();
    let token = registered.issued_token.unwrap();
    let store = Arc::new(store);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let (store, token, barrier) = (Arc::clone(&store), token.clone(), Arc::clone(&barrier));
        workers.push(std::thread::spawn(move || { barrier.wait(); token_with_starting_rules(&store, &token, NOW + 1).unwrap(); }));
    }
    let reward_store = Arc::clone(&store);
    workers.push(std::thread::spawn(move || {
        barrier.wait();
        let job = SaveJob::reward(registered.account, 1, BattleReward::new(registered.account, 5, 25, 2));
        persist(&reward_store, GameId::Ff7, &StartingRules { gil: 240 }, &job, NOW + 1).unwrap();
    }));
    for worker in workers { worker.join().unwrap(); }
    let final_record = store.get_character_progress(registered.account, GameId::Ff7).unwrap().unwrap();
    assert_eq!((final_record.exp, final_record.gil, final_record.ap), (58, 25, 2));
    assert_eq!(crate::inventory::Inventory::resolve(&StartingRules { gil: 240 }, final_record.inventory.as_deref()).entries(),
        entries(&[(0, 3), (3, 1)]).unwrap(), "a reward that creates the record first may retain legacy unstored-stock semantics");
}

#[test]
fn a_new_account_fights_as_the_slot_it_chose_and_has_earned_nothing() {
    let (_dir, store) = store();
    let tifa = register_ff7(&store, "Tifa", [0; 3], None, Some(2)).unwrap();
    assert_eq!(tifa.progress, ProgressSeed::of(2, None));
    // No choice at all: Barret, never Cloud.
    let plain = plain_ff7(&store, "Plain").unwrap();
    assert_eq!(plain.progress, ProgressSeed::of(1, None));
    // A game that has no characters of its own: everybody is the default.
    let fixture = register_for(GameId::Fixture, &store, "Fixy", request([0; 3], None, Some(5))).unwrap();
    assert_eq!(fixture.progress, ProgressSeed::default());
    // Nothing was written for any of them.
    for login in [&tifa, &plain] {
        assert_eq!(store.get_character_progress(login.account, GameId::Ff7).unwrap(), None);
    }
}

#[test]
fn logging_in_loads_the_stored_progress_through_the_pointer_and_the_slot_wins() {
    let (_dir, store) = store();
    let reg = register_ff7(&store, "Barret", [0; 3], None, Some(1)).unwrap();
    let token = reg.issued_token.unwrap();
    // Before anything was earned: the slot's character, nothing stored.
    let first = with_token(&store, &token).unwrap();
    assert_eq!(first.progress, ProgressSeed::of(1, None));

    // After a battle: what the store holds comes back, as the stored numbers.
    let record = fmv_persist::CharacterProgress { exp: 700, ap: 9, gil: 250, ..fmv_persist::CharacterProgress::new(reg.account, GameId::Ff7, 1, 8, NOW) };
    store.put_character_progress(&record).unwrap();
    let again = with_token(&store, &token).unwrap();
    assert_eq!(again.progress.character, 1);
    assert_eq!(again.progress.stored, Some(Progress { character: 1, level: 8, exp: 700, ap: 9, gil: 250, limit: 0 }));
    // It is not the story: the progression is untouched by it.
    assert!(again.progression.is_empty());
    // Another game's record is not this game's.
    store.put_character_progress(&fmv_persist::CharacterProgress::new(reg.account, GameId::Ff8, 0, 40, NOW)).unwrap();
    assert_eq!(with_token(&store, &token).unwrap().progress, again.progress);

    // A record that names another character than the slot (a rolled-back v7 saved it by look): the slot wins at
    // login (the world's mirror fights as the slot), and the numbers are the stored ones, untouched.
    let by_look = fmv_persist::CharacterProgress { exp: 700, ap: 9, gil: 250, ..fmv_persist::CharacterProgress::new(reg.account, GameId::Ff7, 0, 8, NOW + 1) };
    store.put_character_progress(&by_look).unwrap();
    let healed = with_token(&store, &token).unwrap();
    assert_eq!(healed.progress.character, 1, "the slot wins");
    assert_eq!(healed.progress.stored, Some(Progress { character: 0, level: 8, exp: 700, ap: 9, gil: 250, limit: 0 }), "what is stored is passed on as it is");
    let mirror = progress::resolve(&SyntheticRules::default(), &healed.progress);
    assert_eq!(mirror, Progress { character: 1, level: 8, exp: 700, ap: 9, gil: 250, limit: 0 }, "the mirror: the slot's character, the stored numbers");
}

/// A character record that cannot be read fails the login (like a story blob that cannot be read does): a default
/// would be written over it by the next battle, and an operator has to look at it first. The client is told the
/// store failed, nothing about the file.
#[test]
fn a_damaged_character_record_fails_the_login_and_is_left_for_the_operator() {
    let (dir, store) = store();
    let reg = register_ff7(&store, "Cid", [0; 3], None, Some(8)).unwrap();
    let token = reg.issued_token.unwrap();
    store.put_character_progress(&fmv_persist::CharacterProgress::new(reg.account, GameId::Ff7, 8, 7, NOW)).unwrap();
    let file = progress_file(&dir, reg.account);
    std::fs::write(&file, b"{ not json").unwrap();
    let err = with_token(&store, &token).unwrap_err();
    assert!(matches!(err, LoginError::Internal(_)), "{err:?}");
    assert!(!err.client_text().contains("json") && !err.client_text().contains("state"), "{}", err.client_text());
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not json", "the damaged file is still there to be looked at");
}

/// So does a damaged player-character record, and it is not replaced by a migration.
#[test]
fn a_damaged_player_character_record_fails_the_login_and_is_not_replaced() {
    let (dir, store) = store();
    let reg = register_ff7(&store, "Cid", [0; 3], None, Some(8)).unwrap();
    let token = reg.issued_token.unwrap();
    let file = character_file(&dir, reg.account);
    std::fs::write(&file, b"{ not json").unwrap();
    let err = with_token(&store, &token).unwrap_err();
    assert!(matches!(err, LoginError::Internal(_)), "{err:?}");
    assert!(!err.client_text().contains("json") && !err.client_text().contains("state"), "{}", err.client_text());
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not json");
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

/// What stored files can put in front of other players is filtered, in the snapshot and in the card.
#[test]
fn stored_junk_never_reaches_other_players() {
    let (dir, store) = store();
    let reg = register_ff7(&store, "Legacy", [1, 2, 3], Some("slight"), Some(3)).unwrap();
    let token = reg.issued_token.unwrap();
    // A hand-edited record: a path where a binding belongs.
    let file = character_file(&dir, reg.account);
    let text = std::fs::read_to_string(&file).unwrap().replace("ff7/body/slight", "../../etc/passwd");
    std::fs::write(&file, text).unwrap();
    let again = with_token(&store, &token).unwrap();
    assert_eq!(again.avatar.appearance_ref, None);
    assert_eq!(again.card.binding, None);
    assert_eq!(again.avatar.display_name, "Legacy");
    assert_eq!(again.card.slot, Some(3), "the slot is not a look");
}

// ---- accounts of a v6 and a v7 server -----------------------------------------------------------------

/// `progression/1/Ff7.character.json` as the **v6** server last saved it (level 8, four stacks, no `limit` key):
/// the same bytes as the fixture of `fmv-persist`'s tests, which also pin them against the serializer.
const V6_PROGRESS: &str = r#"{
  "schema_version": 1,
  "account_id": 1,
  "game": "Ff7",
  "character": 2,
  "level": 8,
  "exp": 1044,
  "ap": 61,
  "gil": 260,
  "updated_at": 1793500000,
  "inventory": [
    {
      "item": 0,
      "count": 2
    },
    {
      "item": 3,
      "count": 1
    },
    {
      "item": 7,
      "count": 1
    },
    {
      "item": 11,
      "count": 5
    }
  ]
}
"#;

/// The same account's record as the **v7** server writes it once a battle left a limit gauge.
const V7_PROGRESS: &str = r#"{
  "schema_version": 1,
  "account_id": 1,
  "game": "Ff7",
  "character": 2,
  "level": 20,
  "exp": 17820,
  "ap": 330,
  "gil": 9120,
  "updated_at": 1793600000,
  "inventory": [
    {
      "item": 0,
      "count": 9
    },
    {
      "item": 3,
      "count": 4
    }
  ],
  "limit": 137
}
"#;

fn entries(stacks: &[(u16, u16)]) -> Option<Vec<fmv_core::InventoryEntry>> {
    Some(stacks.iter().map(|&(item, count)| fmv_core::InventoryEntry::new(item, count)).collect())
}

/// Every file under `root` as relative path -> bytes (`LOCK` excluded).
fn file_map(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel != "LOCK" {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A directory as a server of the build before protocol v8 left it: Blizz, account 1, with the look `look` (if any)
/// and `progress` as the record of its last battle. Returns what logging in needs.
fn legacy_directory(look: Option<&str>, progress_json: &str) -> (tempfile::TempDir, Store, String) {
    let (dir, store) = store();
    let reg = store.register("Blizz", [90, 125, 245], DEFAULT_ARCHETYPE, look.map(|l| (GameId::Ff7, l)), NOW).unwrap();
    assert_eq!(reg.account_id, AccountId(1));
    assert!(reg.character.is_none(), "the builds before v8 made no character record");
    // The first login of every account records itself in the index log (that is the build before's doing too), so
    // it is done here, before the files are compared.
    store.authenticate(&reg.token).unwrap();
    std::fs::create_dir_all(progress_file(&dir, AccountId(1)).parent().unwrap()).unwrap();
    std::fs::write(progress_file(&dir, AccountId(1)), progress_json).unwrap();
    (dir, store, reg.token)
}

/// What logging in as the migrated account must give, whatever the account's numbers are: the same numbers in
/// the seed, and not one file of the older build touched.
fn check_migration(dir: &tempfile::TempDir, store: &Store, token: &str, progress_json: &str, expected: Progress, items: Option<Vec<fmv_core::InventoryEntry>>) -> Login {
    let root = dir.path().join("state");
    let before = file_map(&root);
    let first = with_token(store, token).unwrap();
    let after = file_map(&root);
    for (name, bytes) in &before {
        assert_eq!(after.get(name), Some(bytes), "{name} changed: a rolled-back build must find what it wrote");
    }
    let added: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert_eq!(added, ["characters/1.json"], "logging in only adds the player-character record");
    assert_eq!(std::fs::read_to_string(progress_file(dir, AccountId(1))).unwrap(), progress_json);

    // Every number of the seed is the stored one, and the mirror the world makes of it agrees.
    assert_eq!(first.progress.stored, Some(expected));
    assert_eq!(first.progress.inventory, items);
    assert_eq!(first.progress.character, expected.character, "it fights as it did");
    let mirror = progress::resolve(&SyntheticRules::default(), &first.progress);
    assert_eq!(mirror, expected, "the mirror of the account is the stored record");
    assert!(first.card.migrated);
    assert_eq!(first.card.slot, Some(expected.character));
    assert!(first.progression.is_empty(), "the account never advanced a story");

    // Logging in again changes nothing and gives the same.
    let second = with_token(store, token).unwrap();
    assert_eq!((second.progress, second.card, second.avatar), (first.progress.clone(), first.card.clone(), first.avatar.clone()));
    assert_eq!(file_map(&root), after);
    first
}

#[test]
fn a_v6_shaped_account_migrates_without_changing_any_number() {
    let (dir, store, token) = legacy_directory(Some("ff7/tifa"), V6_PROGRESS);
    let expected = Progress { character: 2, level: 8, exp: 1044, ap: 61, gil: 260, limit: 0 };
    let login = check_migration(&dir, &store, &token, V6_PROGRESS, expected, entries(&[(0, 2), (3, 1), (7, 1), (11, 5)]));
    // It is Tifa's slot, and it keeps the Tifa model it chose: its look is the binding.
    assert_eq!(login.card.binding.as_deref(), Some("ff7/tifa"));
    assert_eq!(login.avatar.appearance_ref.as_deref(), Some("ff7/tifa"));
    assert_eq!(login.avatar.tint, [90, 125, 245]);
    assert_eq!(login.card.body, None, "no body was ever chosen");
    // The pointer finds the record that is there.
    let record = store.get_player_character(AccountId(1)).unwrap().unwrap();
    assert_eq!(record.slot(GameId::Ff7), Some(2));
    assert_eq!(store.follow_character_progress(&record, GameId::Ff7).unwrap().map(|l| (l.record.level, l.in_sync())), Some((8, true)));
}

#[test]
fn a_v7_shaped_account_migrates_without_changing_any_number() {
    let (dir, store, token) = legacy_directory(Some("ff7/tifa"), V7_PROGRESS);
    let expected = Progress { character: 2, level: 20, exp: 17_820, ap: 330, gil: 9_120, limit: 137 };
    let login = check_migration(&dir, &store, &token, V7_PROGRESS, expected, entries(&[(0, 9), (3, 4)]));
    assert_eq!(login.progress.stored.map(|p| p.limit), Some(137), "the limit gauge is the stored one");
}

/// No look at all (what a headless bot has): the character the builds before fought it as is Cloud, and slot 0 is
/// allowed for a migrated account. It is not drawn as Cloud any more: it never chose that look, so it gets the default
/// body.
#[test]
fn a_migrated_account_without_a_look_keeps_cloud_as_its_slot_but_not_as_its_model() {
    let v7_cloud = V7_PROGRESS.replace("\"character\": 2", "\"character\": 0");
    let (dir, store, token) = legacy_directory(None, &v7_cloud);
    let expected = Progress { character: 0, level: 20, exp: 17_820, ap: 330, gil: 9_120, limit: 137 };
    let login = check_migration(&dir, &store, &token, &v7_cloud, expected, entries(&[(0, 9), (3, 4)]));
    let default_body = fmv_ff7::playable::body_or_default(None).model.appearance_ref;
    assert_eq!(login.card.binding.as_deref(), Some(default_body));
    assert_eq!(login.card.slot, Some(0));
    assert_eq!(login.avatar.appearance_ref.as_deref(), Some(default_body));
}

/// The look decides the slot of a migrated account, as it did: a record that names another character is left
/// alone, and the slot (the character the builds before fought it as) wins in the mirror.
#[test]
fn a_migrated_accounts_slot_is_the_one_its_look_named_not_the_one_its_record_holds() {
    let odd = V6_PROGRESS.replace("\"character\": 2", "\"character\": 0");
    let (dir, store, token) = legacy_directory(Some("ff7/tifa"), &odd);
    let root = dir.path().join("state");
    let before = file_map(&root);
    let login = with_token(&store, &token).unwrap();
    assert_eq!(login.card.slot, Some(2), "Tifa: what v7 fought it as, whatever the record says");
    assert_eq!(login.progress.character, 2);
    assert_eq!(login.progress.stored.map(|p| (p.character, p.level, p.exp)), Some((0, 8, 1044)), "the stored numbers, untouched");
    let mirror = progress::resolve(&SyntheticRules::default(), &login.progress);
    assert_eq!((mirror.character, mirror.level, mirror.exp), (2, 8, 1044));
    let after = file_map(&root);
    assert_eq!(after.get("progression/1/Ff7.character.json"), before.get("progression/1/Ff7.character.json"));
}

// ---- a win, a restart, and the file still says who fought ------------------------------------------------

/// A new character that holds Barret's slot wins a battle, the server restarts, and its progress file still names
/// Barret and its level is unchanged: the saver writes the character of the SLOT (the world queues the member's
/// character), never the one a look would name (a generic body names none: that would be Cloud). Run with the
/// rules and the store of a server, over a state directory that is closed and opened again between the two.
#[test]
fn a_new_barret_slot_character_keeps_barret_and_its_level_through_a_win_and_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let rules: Arc<dyn BattleRules> = Arc::new(SyntheticRules::default());
    let (account, token) = {
        let store = Store::open(&root).unwrap();
        let reg = register_ff7(&store, "Wedge", [200, 10, 10], Some("sturdy"), Some(1)).unwrap();
        assert_eq!(reg.progress.character, 1, "the seed the world mirrors is Barret's");
        // The world queues a payment for the character that fought: the seed's, i.e. the slot's.
        let job = SaveJob::reward(reg.account, reg.progress.character, BattleReward::new(reg.account, 12, 20, 2));
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job, NOW).unwrap();
        assert_eq!(saved.progress, Progress { character: 1, level: 5, exp: 52, ap: 2, gil: 20, limit: 0 });
        let job = SaveJob::reward(reg.account, reg.progress.character, BattleReward::new(reg.account, 30, 5, 1));
        let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job, NOW + 1).unwrap();
        assert_eq!((saved.progress.character, saved.progress.level, saved.progress.exp), (1, 6, 82), "52 + 30 passes level 6 (60)");
        (reg.account, reg.issued_token.unwrap())
    };
    // The file names Barret.
    let text = std::fs::read_to_string(progress_file(&dir, account)).unwrap();
    for line in ["\"character\": 1,", "\"level\": 6,", "\"exp\": 82,"] {
        assert!(text.contains(line), "{line} is not in the progress file: {text}");
    }

    // The server restarts (the store is closed and opened on the same directory) and the player logs in.
    let store = Store::open(&root).unwrap();
    let back = with_token(&store, &token).unwrap();
    assert_eq!(back.card.slot, Some(1));
    assert_eq!(back.progress.character, 1);
    assert_eq!(back.progress.stored, Some(Progress { character: 1, level: 6, exp: 82, ap: 3, gil: 25, limit: 0 }), "its level is unchanged");
    let mirror = progress::resolve(rules.as_ref(), &back.progress);
    assert_eq!((mirror.character, mirror.level, mirror.exp), (1, 6, 82));
    // Another win after the restart adds on and the file still says Barret.
    let job = SaveJob::reward(account, back.progress.character, BattleReward::new(account, 5, 0, 0));
    let saved = persist(&store, GameId::Ff7, rules.as_ref(), &job, NOW + 2).unwrap();
    assert_eq!((saved.progress.character, saved.progress.exp), (1, 87));
    let text = std::fs::read_to_string(progress_file(&dir, account)).unwrap();
    assert!(text.contains("\"character\": 1,") && text.contains("\"exp\": 87,"), "{text}");
}
