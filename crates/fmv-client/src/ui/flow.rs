//! The glue between the menu and the rest of the client: listing the saved characters, turning a
//! menu action into a login plan and a network configuration, and turning a failure into a
//! message for the screen. Everything here is a plain function over values (and a token store),
//! so it is tested without a window: `tests::enter_on_the_create_form_registers_on_the_wire` runs
//! a whole registration, from the key presses to the `Hello` the server receives.

use super::looks::LookCache;
use super::menu::{CharacterEntry, Look, MenuAction, Notice};
use crate::accounts::{SavedAccount, TokenStore};
use crate::cli::tint_from_name;
use crate::login::{plan_login, LoginPlan};
use crate::net::{clean, NetConfig, NetError, SessionTiming, TokenSink};
use fmv_protocol::RejectCode;

/// What a connection attempt needs to know about this client.
pub struct Env<'a> {
    pub server: &'a str,
    pub store: &'a TokenStore,
    pub looks: &'a LookCache,
    pub client_build: &'a str,
}

/// The saved characters of the server: the accounts to log in with, the same list as the menu
/// shows it (same order), and a line for each token file that could not be used.
#[derive(Debug, Clone, Default)]
pub struct Saved {
    pub accounts: Vec<SavedAccount>,
    pub entries: Vec<CharacterEntry>,
    pub problems: Vec<String>,
}

/// Read the token store and the look notes.
pub fn saved_characters(env: &Env<'_>) -> Saved {
    let list = env.store.list(env.server);
    let entries = list
        .accounts
        .iter()
        .map(|a| CharacterEntry {
            name: clean(&a.name),
            look: env.looks.load(env.server, &a.name),
            loose_permissions: a.loose_mode.is_some(),
        })
        .collect();
    Saved { accounts: list.accounts, entries, problems: list.problems }
}

/// A connection attempt, ready to start.
pub struct Attempt {
    pub plan: LoginPlan,
    pub cfg: NetConfig,
}

/// Wrap the sink that saves a registration's token so that it also notes the look the character
/// was registered with (display only; a failure to write the note is never the player's problem).
fn noting_sink(inner: TokenSink, looks: LookCache, server: String, name: String, look: Look) -> TokenSink {
    TokenSink::new(move |token| {
        inner.save(token)?;
        let _ = looks.store(&server, &name, &look);
        Ok(())
    })
}

/// Turn a menu action that starts a connection (`Play` or `Register`) into the login plan and the
/// network configuration for it. `accounts` are the saved accounts the menu's indices refer to.
/// The `Err` is a message for the screen; nothing has been sent or written yet.
pub fn prepare(env: &Env<'_>, accounts: &[SavedAccount], action: &MenuAction) -> Result<Attempt, Notice> {
    let config = |plan: &LoginPlan, tint: [u8; 3], appearance_ref: Option<String>, token_sink: TokenSink| NetConfig {
        server: env.server.to_string(),
        auth: plan.auth(),
        tint,
        appearance_ref,
        client_build: env.client_build.to_string(),
        timing: SessionTiming::DEFAULT,
        token_sink,
    };
    match action {
        MenuAction::Play { index } => {
            let Some(account) = accounts.get(*index) else {
                return Err(Notice::error("That saved character is gone.").with_hint("Pick another one from the list."));
            };
            let plan = LoginPlan::Token(account.clone());
            // A token login ignores the tint and the look: the server uses the stored profile.
            let cfg = config(&plan, tint_from_name(plan.name()), None, plan.token_sink(env.store, env.server));
            Ok(Attempt { plan, cfg })
        }
        MenuAction::Register { name, tint, appearance } => {
            // `plan_login` also checks that the token can be kept (the directory is writable, a
            // file name is free) BEFORE an account exists whose token would be lost.
            let plan = match plan_login(env.server, Some(name), false, env.store) {
                Ok(plan @ LoginPlan::Register { .. }) => plan,
                Ok(LoginPlan::Token(_)) => {
                    return Err(Notice::error(format!("You already have a saved character called {name} on this server."))
                        .with_hint("Press Esc and pick it from the list, or choose another name."))
                }
                Err(text) => return Err(Notice::error(text)),
            };
            let look = Look { tint: *tint, appearance: appearance.clone() };
            let sink = noting_sink(
                plan.token_sink(env.store, env.server),
                env.looks.clone(),
                env.server.to_string(),
                name.clone(),
                look,
            );
            let cfg = config(&plan, *tint, appearance.clone(), sink);
            Ok(Attempt { plan, cfg })
        }
        MenuAction::Quit | MenuAction::Forget { .. } | MenuAction::CancelConnect => {
            Err(Notice::error("That is not a connection."))
        }
    }
}

/// The advice for a failure nobody can do anything specific about.
const TRY_AGAIN: &str = "Press Enter to try again.";

/// The message for a failed attempt. The server's own words are always shown (cleaned, they are
/// text from the network), followed by what the player can do about it. For a refusal that advice
/// comes from the server's [`RejectCode`] alone (see `reject_hint`), never from its words,
/// which are free to change. A token that could not be saved is carried as a secret: it is the
/// player's only chance to keep the account.
pub fn failure_notice(error: &NetError, plan: &LoginPlan) -> Notice {
    match error {
        NetError::TokenNotSaved { reason, token } => Notice::error(format!(
            "The character was created on the server, but its login could not be saved on this computer ({reason})."
        ))
        .with_hint("Write the token below into a file by hand: it is shown only once.")
        .with_secret(token.expose()),
        NetError::Rejected { code, reason } => {
            Notice::error(format!("The server refused: {reason}")).with_hint(reject_hint(*code, plan))
        }
        NetError::Content(text) => Notice::error(format!("Cannot join: {text}"))
            .with_hint("This computer's game files must be the same edition as the server's."),
        NetError::Connect { .. } | NetError::ConnectTimeout(..) | NetError::Timeout(_) | NetError::Io { .. } => {
            Notice::error(error.to_string()).with_hint("Check the server address and that the server is running, then press Enter.")
        }
        NetError::Protocol(_) | NetError::Runtime(_) => Notice::error(error.to_string()).with_hint(TRY_AGAIN),
    }
}

/// What the player can do about a refusal, decided from the code alone. The match is exhaustive
/// on purpose, without a catch-all arm: a code added to the protocol has to be given its advice
/// here before this client builds. A server that sends no code (or one this build does not know)
/// is `Unspecified` and gets the generic advice; its words are never read for a clue.
fn reject_hint(code: RejectCode, plan: &LoginPlan) -> &'static str {
    match code {
        RejectCode::NameTaken => "Change the name and press Enter again.",
        RejectCode::InvalidName => "Type a name and press Enter again.",
        RejectCode::RegistrationClosed => "This server takes no new characters. Press Esc and play a saved one.",
        RejectCode::RateLimited => "Wait a minute, then press Enter again.",
        RejectCode::AlreadyOnline => "That character is online elsewhere. Close it there, then press Enter again.",
        // Only a saved login can be stale: a registration sends no token for the server to refuse.
        RejectCode::InvalidToken => match plan {
            LoginPlan::Token(_) => {
                "The server does not know this saved login (its accounts may have been reset). Press Del to forget it, then N to create a new character."
            }
            LoginPlan::Register { .. } => TRY_AGAIN,
        },
        RejectCode::ServerFull => "The server is full. Wait a moment, then press Enter again.",
        RejectCode::TooManyConnections => {
            "Too many connections from this computer or network. Close other copies of the game, wait a moment, then press Enter again."
        }
        RejectCode::ProtocolMismatch => "This client and the server speak different protocol versions: update the older one.",
        RejectCode::ShuttingDown => "The server is restarting. Wait a moment, then press Enter again.",
        RejectCode::Timeout
        | RejectCode::IdleTimeout
        | RejectCode::Internal
        | RejectCode::ContentMismatch
        | RejectCode::BadRequest
        | RejectCode::Unspecified => TRY_AGAIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::handshake;
    use crate::ui::menu::{KeyPress, Menu, MenuConfig, NavKey, NoticeKind, Screen};
    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos, SessionId};
    use fmv_fixture::FixtureAdapter;
    use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, Secret, ServerMsg, PROTOCOL_VERSION};
    use std::fs;
    use std::time::Duration;
    use tokio::io::{duplex, split};

    const SERVER: &str = "127.0.0.1:47770";

    struct Fixture {
        _dir: tempfile::TempDir,
        store: TokenStore,
        looks: LookCache,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path());
        let looks = LookCache::new(dir.path());
        Fixture { _dir: dir, store, looks }
    }

    impl Fixture {
        fn env(&self) -> Env<'_> {
            Env { server: SERVER, store: &self.store, looks: &self.looks, client_build: "test-build" }
        }
    }

    // ---- listing ------------------------------------------------------------------------------

    #[test]
    fn saved_characters_pair_each_login_with_its_remembered_look() {
        let f = fixture();
        f.store.save_new(SERVER, "Cloud", &Secret::new("tok_cloud")).unwrap();
        f.store.save_new(SERVER, "Tifa", &Secret::new("tok_tifa")).unwrap();
        f.looks.store(SERVER, "Cloud", &Look { tint: [1, 2, 3], appearance: Some("ff7/cloud".into()) }).unwrap();
        let saved = saved_characters(&f.env());
        assert_eq!(saved.accounts.len(), 2);
        assert_eq!(saved.entries.len(), 2);
        assert_eq!(saved.entries[0].name, "Cloud");
        assert_eq!(saved.entries[0].look, Some(Look { tint: [1, 2, 3], appearance: Some("ff7/cloud".into()) }));
        assert_eq!(saved.entries[1].name, "Tifa");
        assert_eq!(saved.entries[1].look, None, "never seen: look unknown");
        assert!(saved.problems.is_empty());
        // The entries never contain a token.
        assert!(!format!("{:?}", saved.entries).contains("tok_"));
        // Nothing saved is an empty list, not an error.
        let empty = fixture();
        let none = saved_characters(&empty.env());
        assert!(none.accounts.is_empty() && none.entries.is_empty() && none.problems.is_empty());
    }

    #[test]
    fn a_name_from_a_token_file_is_cleaned_for_display() {
        let f = fixture();
        // A hand-edited file with an invisible character in the name.
        f.store.save_new(SERVER, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let path = f.store.list(SERVER).accounts[0].path.clone();
        fs::write(&path, "tok_cloud\nname=Cl\u{200b}oud\n").unwrap();
        let saved = saved_characters(&f.env());
        assert_eq!(saved.entries[0].name, "Cloud");
    }

    // ---- preparing an attempt -------------------------------------------------------------------

    #[test]
    fn playing_a_saved_character_logs_in_with_its_token_and_changes_nothing_on_disk() {
        let f = fixture();
        f.store.save_new(SERVER, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let saved = saved_characters(&f.env());
        let attempt = prepare(&f.env(), &saved.accounts, &MenuAction::Play { index: 0 }).unwrap();
        assert!(matches!(attempt.plan, LoginPlan::Token(ref a) if a.name == "Cloud"));
        assert_eq!(attempt.cfg.auth, Auth::Token(Secret::new("tok_cloud")));
        assert_eq!(attempt.cfg.server, SERVER);
        assert_eq!(attempt.cfg.client_build, "test-build");
        assert_eq!(attempt.cfg.appearance_ref, None);
        // The sink of a login ignores a token that arrives anyway (it never overwrites).
        attempt.cfg.token_sink.save(&Secret::new("tok_rogue")).unwrap();
        assert_eq!(f.store.find(SERVER, None).unwrap().unwrap().token.expose(), "tok_cloud");
        assert!(!format!("{:?}", attempt.cfg).contains("tok_cloud"), "the config never prints the token");
    }

    #[test]
    fn a_gone_character_is_a_message_not_a_panic() {
        let f = fixture();
        let err = prepare(&f.env(), &[], &MenuAction::Play { index: 3 }).err().expect("an error");
        assert_eq!(err.kind, NoticeKind::Error);
        assert!(err.text.contains("gone"));
        for other in [MenuAction::Quit, MenuAction::CancelConnect, MenuAction::Forget { index: 0 }] {
            assert!(prepare(&f.env(), &[], &other).is_err(), "{other:?} is not a connection");
        }
    }

    #[test]
    fn registering_sends_the_cleaned_name_the_tint_and_the_look_and_saves_the_token_where_planned() {
        let f = fixture();
        let action = MenuAction::Register { name: "Cloud".into(), tint: [10, 20, 30], appearance: Some("ff7/tifa".into()) };
        let attempt = prepare(&f.env(), &[], &action).unwrap();
        assert!(matches!(attempt.plan, LoginPlan::Register { ref name, replace: false } if name == "Cloud"));
        assert_eq!(attempt.cfg.auth, Auth::Register { display_name: "Cloud".into() });
        assert_eq!(attempt.cfg.tint, [10, 20, 30]);
        assert_eq!(attempt.cfg.appearance_ref.as_deref(), Some("ff7/tifa"));
        // When the server's Welcome arrives the sink saves the token and notes the look.
        attempt.cfg.token_sink.save(&Secret::new("fmvt_new")).unwrap();
        let saved = f.store.find(SERVER, Some("Cloud")).unwrap().unwrap();
        assert_eq!(saved.token.expose(), "fmvt_new");
        assert_eq!(f.looks.load(SERVER, "Cloud"), Some(Look { tint: [10, 20, 30], appearance: Some("ff7/tifa".into()) }));
        // The list now shows the new character with its look.
        let list = saved_characters(&f.env());
        assert_eq!(list.entries[0].name, "Cloud");
        assert_eq!(list.entries[0].look.as_ref().map(|l| l.tint), Some([10, 20, 30]));
    }

    #[test]
    fn a_registration_is_refused_up_front_when_its_token_could_not_be_kept() {
        let f = fixture();
        // The accounts directory is a file: no token could be saved, so no account may be created.
        fs::write(f._dir.path().join(crate::accounts::ACCOUNTS_DIR), "in the way").unwrap();
        let action = MenuAction::Register { name: "Cloud".into(), tint: [1, 2, 3], appearance: None };
        let err = prepare(&f.env(), &[], &action).err().expect("refused");
        assert_eq!(err.kind, NoticeKind::Error);
        assert!(err.text.contains("accounts"), "it says what is wrong: {}", err.text);
    }

    #[test]
    fn a_name_that_became_a_saved_login_meanwhile_is_not_registered_again() {
        let f = fixture();
        f.store.save_new(SERVER, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let action = MenuAction::Register { name: "cloud".into(), tint: [1, 2, 3], appearance: None };
        let err = prepare(&f.env(), &[], &action).err().expect("refused");
        assert!(err.text.contains("already have a saved character"), "{}", err.text);
        assert_eq!(f.store.find(SERVER, None).unwrap().unwrap().token.expose(), "tok_cloud", "untouched");
    }

    // ---- the whole registration, on the wire ----------------------------------------------------

    fn snapshot_of(adapter: &FixtureAdapter, entities: Vec<EntitySnapshot>) -> ServerMsg {
        ServerMsg::AreaSnapshot { area: adapter.list_areas().remove(0).id, tick: 1, entities, npcs: vec![] }
    }

    /// Press the keys of a new player (type the name, move to the look, pick the second one, Enter)
    /// and run the resulting registration through the real handshake against a scripted server.
    /// The server checks the `Hello` it receives: this is what "Enter = register" means on the wire.
    #[tokio::test]
    async fn enter_on_the_create_form_registers_on_the_wire() {
        let f = fixture();
        let mut menu = Menu::new(MenuConfig::new(SERVER, GameId::Ff7), vec![]);
        menu.text("  Tifa ");
        menu.key(KeyPress::new(NavKey::Tab)); // look
        menu.key(KeyPress::new(NavKey::Right)); // ff7/tifa
        menu.key(KeyPress::new(NavKey::Tab)); // palette
        menu.key(KeyPress::new(NavKey::Right));
        let chosen = match menu.screen() {
            Screen::Create(form) => form.tint,
            other => panic!("{other:?}"),
        };
        let action = menu.key(KeyPress::new(NavKey::Enter)).expect("Enter registers");
        let attempt = prepare(&f.env(), &[], &action).unwrap();

        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let server_adapter = FixtureAdapter::new();
        let expected_tint = chosen;
        let script = tokio::spawn(async move {
            let ClientMsg::Hello { protocol_version, client_build, auth, tint, appearance_ref } =
                read_msg(&mut srd).await.expect("a Hello")
            else {
                panic!("the first message must be Hello")
            };
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(client_build, "test-build");
            assert_eq!(auth, Auth::Register { display_name: "Tifa".into() }, "the sanitised name");
            assert_eq!(tint, expected_tint, "the chosen tint");
            assert_eq!(appearance_ref.as_deref(), Some("ff7/tifa"), "the chosen look");
            let info = server_adapter.list_areas().remove(0);
            write_msg(
                &mut swr,
                &ServerMsg::Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    server_build: "srv".into(),
                    session: SessionId(1),
                    entity: EntityId(7),
                    account: AccountId(42),
                    issued_token: Some(Secret::new("fmvt_issued_by_the_server")),
                },
            )
            .await
            .unwrap();
            write_msg(
                &mut swr,
                &ServerMsg::AreaManifest {
                    area: info.id.clone(),
                    display_name: info.display_name.clone(),
                    fingerprint: info.fingerprint,
                    provenance: info.provenance.summary(),
                    fixture: info.fixture,
                },
            )
            .await
            .unwrap();
            let join: ClientMsg = read_msg(&mut srd).await.unwrap();
            assert!(matches!(join, ClientMsg::JoinArea { .. }), "{join:?}");
            let me = EntitySnapshot {
                id: EntityId(7),
                pos: Pos::new(0.0, 0.0, 0.0),
                facing: 0.0,
                anim: AnimState::Idle,
                avatar: AvatarAppearance::new("Tifa", expected_tint, Some("ff7/tifa".into())),
            };
            write_msg(&mut swr, &snapshot_of(&server_adapter, vec![me])).await.unwrap();
        });
        let joined = handshake(&mut crd, &mut cwr, &attempt.cfg, &adapter, Duration::from_secs(5)).await.expect("joined");
        script.await.expect("the scripted server saw what it expected");
        assert_eq!(joined.account, AccountId(42));
        // The token the server issued was saved under the right name, at once, and the look noted.
        let saved = f.store.find(SERVER, Some("Tifa")).unwrap().expect("the token was saved");
        assert_eq!(saved.token.expose(), "fmvt_issued_by_the_server");
        let look = f.looks.load(SERVER, "Tifa").expect("the look was noted");
        assert_eq!((look.tint, look.appearance.as_deref()), (chosen, Some("ff7/tifa")));
    }

    /// A refused registration comes back as a message and the form is ready for another try.
    #[tokio::test]
    async fn a_name_that_is_taken_comes_back_to_the_form_with_the_servers_words() {
        let f = fixture();
        let mut menu = Menu::new(MenuConfig::new(SERVER, GameId::Ff7), vec![]);
        menu.text("Cloud");
        let action = menu.key(KeyPress::new(NavKey::Enter)).expect("register");
        let attempt = prepare(&f.env(), &[], &action).unwrap();
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(16 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let script = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &ServerMsg::reject(RejectCode::NameTaken, "that display name is already taken")).await.unwrap();
        });
        let error = handshake(&mut crd, &mut cwr, &attempt.cfg, &adapter, Duration::from_secs(5)).await.err().expect("refused");
        script.await.unwrap();
        // Nothing was saved: no account exists on this side.
        assert!(f.store.list(SERVER).accounts.is_empty());
        menu.refresh_accounts(saved_characters(&f.env()).entries);
        menu.connect_failed(failure_notice(&error, &attempt.plan));
        let Screen::Create(form) = menu.screen() else { panic!("back on the form: {:?}", menu.screen()) };
        assert_eq!(form.name.text(), "Cloud");
        let notice = menu.notice().expect("a message");
        assert!(notice.text.contains("that display name is already taken"), "the server's own words: {notice:?}");
        assert_eq!(notice.hint.as_deref(), Some("Change the name and press Enter again."));
    }

    // ---- failure messages -----------------------------------------------------------------------

    fn register_plan() -> LoginPlan {
        LoginPlan::Register { name: "Cloud".into(), replace: false }
    }

    fn token_plan() -> LoginPlan {
        let f = fixture();
        f.store.save_new(SERVER, "Cloud", &Secret::new("tok_cloud")).unwrap();
        LoginPlan::Token(f.store.list(SERVER).accounts.remove(0))
    }

    fn refused(code: RejectCode, reason: &str) -> NetError {
        NetError::Rejected { code, reason: reason.to_string() }
    }

    #[test]
    fn every_refusal_shows_the_servers_words_and_a_way_forward() {
        let register = register_plan();
        // (the server's code, its words, a fragment of the advice). Every code has a row.
        let table = [
            (RejectCode::NameTaken, "that display name is already taken", "Change the name"),
            (RejectCode::InvalidName, "display name must not be empty", "Type a name"),
            (RejectCode::RegistrationClosed, "registration is closed on this server; log in with a saved token", "no new characters"),
            (RejectCode::RateLimited, "too many registrations on this server right now; try again in a minute", "Wait a minute"),
            (RejectCode::RateLimited, "rate limit exceeded", "Wait a minute"),
            (RejectCode::AlreadyOnline, "this account is already logged in", "online elsewhere"),
            // A registration sends no token, so a refused token has no saved login to blame (the
            // saved-token plan has its own test below).
            (RejectCode::InvalidToken, "invalid or unknown token", "Press Enter to try again"),
            (RejectCode::ServerFull, "server full", "The server is full"),
            (RejectCode::TooManyConnections, "too many connections from your address", "Too many connections"),
            (RejectCode::ProtocolMismatch, "protocol version mismatch", "different protocol versions"),
            (RejectCode::ShuttingDown, "the server is shutting down", "restarting"),
            (RejectCode::Timeout, "no Hello within 10 s", "Press Enter to try again"),
            (RejectCode::IdleTimeout, "session closed for inactivity", "Press Enter to try again"),
            (RejectCode::Internal, "internal error", "Press Enter to try again"),
            (RejectCode::ContentMismatch, "area fingerprint is not the server's", "Press Enter to try again"),
            (RejectCode::BadRequest, "malformed message", "Press Enter to try again"),
            (RejectCode::Unspecified, "some reason nobody foresaw", "Press Enter to try again"),
            // The hint comes from the code and NOT from the words: another sentence with the same
            // code gets the same advice...
            (RejectCode::NameTaken, "computer says no", "Change the name"),
            (RejectCode::RateLimited, "that display name is already taken", "Wait a minute"),
            // ...and the words of an old server (no code) get nothing but the generic advice.
            (RejectCode::Unspecified, "that display name is already taken", "Press Enter to try again"),
            (RejectCode::Unspecified, "registration is closed on this server", "Press Enter to try again"),
            (RejectCode::Unspecified, "this account is already logged in", "Press Enter to try again"),
            (RejectCode::Unspecified, "rate limit exceeded", "Press Enter to try again"),
        ];
        for (code, reason, hint) in table {
            let n = failure_notice(&refused(code, reason), &register);
            assert_eq!(n.kind, NoticeKind::Error);
            assert_eq!(n.text, format!("The server refused: {reason}"), "{code:?}");
            assert!(n.hint.as_deref().is_some_and(|h| h.contains(hint)), "{code:?} {reason}: {n:?}");
            assert!(n.secret.is_none());
        }
        // The generic advice is exactly that, not some other sentence that happens to contain it.
        for (code, reason, hint) in table {
            if hint == "Press Enter to try again" {
                let n = failure_notice(&refused(code, reason), &register);
                assert_eq!(n.hint.as_deref(), Some("Press Enter to try again."), "{code:?} {reason}");
            }
        }
        // No code is left without a row, so a code added to the protocol shows up here too.
        let covered: std::collections::HashSet<RejectCode> = table.iter().map(|(code, ..)| *code).collect();
        for code in RejectCode::ALL {
            assert!(covered.contains(&code), "{code:?} has no row in the table");
        }
        // Whatever the code, the notice is an error that quotes the server and says what to do.
        let saved = token_plan();
        for plan in [&register, &saved] {
            for code in RejectCode::ALL {
                let n = failure_notice(&refused(code, "words of the server"), plan);
                assert_eq!((n.kind, n.text.as_str()), (NoticeKind::Error, "The server refused: words of the server"), "{code:?}");
                assert!(n.hint.as_deref().is_some_and(|h| !h.is_empty()), "{code:?}: {n:?}");
            }
        }
    }

    #[test]
    fn a_stale_saved_token_points_at_forget_and_new_character() {
        let stale = refused(RejectCode::InvalidToken, "invalid or unknown token");
        let saved = token_plan();
        let n = failure_notice(&stale, &saved);
        assert!(n.text.contains("invalid or unknown token"));
        let hint = n.hint.unwrap();
        assert!(hint.contains("Del") && hint.contains("forget") && hint.contains("N to create"), "{hint}");
        // The code points at the saved login, not the sentence: any words do for InvalidToken...
        let other_words = failure_notice(&refused(RejectCode::InvalidToken, "we lost your account"), &saved);
        assert_eq!(other_words.hint.as_deref(), Some(hint.as_str()));
        // ...and the very same words with another code do not.
        for code in RejectCode::ALL.into_iter().filter(|code| *code != RejectCode::InvalidToken) {
            let n = failure_notice(&refused(code, "invalid or unknown token"), &saved);
            let other = n.hint.unwrap();
            assert!(other != hint && !other.contains("forget"), "{code:?}: {other}");
        }
        // For a registration the same refusal has no saved token to blame.
        let n = failure_notice(&stale, &register_plan());
        assert_eq!(n.hint.as_deref(), Some("Press Enter to try again."));
    }

    #[test]
    fn a_token_that_could_not_be_saved_is_a_secret_alert_and_its_text_never_holds_the_token() {
        let error = NetError::TokenNotSaved { reason: "disk full".into(), token: Secret::new("fmvt_precious") };
        let n = failure_notice(&error, &register_plan());
        assert!(n.is_alert());
        assert_eq!(n.secret.as_ref().map(Secret::expose), Some("fmvt_precious"));
        assert!(n.text.contains("disk full"));
        assert!(!n.text.contains("fmvt_precious") && !n.hint.clone().unwrap_or_default().contains("fmvt_precious"));
        assert!(!format!("{n:?}").contains("fmvt_precious"), "Debug redacts it: {n:?}");
    }

    #[test]
    fn connection_problems_say_what_failed_and_to_try_again() {
        let plan = register_plan();
        let connect = NetError::Connect {
            addr: SERVER.into(),
            source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
        };
        for error in [
            connect,
            NetError::ConnectTimeout(SERVER.into(), 5),
            NetError::Timeout("Welcome"),
            NetError::Io { during: "Welcome", detail: "the server closed the connection".into() },
        ] {
            let n = failure_notice(&error, &plan);
            assert!(n.text.contains(&error.to_string()), "{n:?}");
            assert!(n.hint.as_deref().is_some_and(|h| h.contains("server")), "{n:?}");
        }
        let n = failure_notice(&NetError::Content("this machine's FF7 copy differs".into()), &plan);
        assert!(n.text.contains("Cannot join") && n.text.contains("differs"));
        let n = failure_notice(&NetError::Protocol("server speaks protocol v9".into()), &plan);
        assert!(n.text.contains("protocol v9"));
    }
}
