//! Tests of the delete exchange (over in-memory pipes and over a real socket) and of the `--delete` command. Every file
//! is in a temporary directory; no game data is read or written.

use super::*;
use crate::ui::menu::Look;
use crate::ui::testkit::{serve_delete, DeleteReply};
use fmv_core::EntityId;
use fmv_protocol::{write_msg, ProtoError};
use std::future::Future;
use tokio::io::{duplex, split, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

const TOKEN: &str = "fmvt_the_login_of_a_character_1234567890";
const BUDGET: Duration = Duration::from_secs(5);

fn token() -> Secret {
    Secret::new(TOKEN)
}

/// The request this client must send, to the letter.
fn expected_request() -> ClientMsg {
    ClientMsg::DeleteCharacter { protocol_version: PROTOCOL_VERSION, client_build: "test-build".into(), token: token() }
}

/// Run [`delete_character`] against `server`, a scripted peer that is handed its two halves of the pipe. Returns the
/// client's result and what the server returned; the client's halves are dropped (a hang-up) before the server is awaited.
async fn exchange<S, F, T>(budget: Duration, server: S) -> (Result<DeleteOutcome, NetError>, T)
where
    S: FnOnce(ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) -> F,
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (client, peer) = duplex(64 * 1024);
    let (mut crd, mut cwr) = split(client);
    let (srd, swr) = split(peer);
    let task = tokio::spawn(server(srd, swr));
    let result = delete_character(&mut crd, &mut cwr, &token(), "test-build", budget).await;
    drop((crd, cwr));
    (result, task.await.expect("the scripted server ran to its end"))
}

/// A server that reads the request and answers with `answer`.
async fn answered(answer: ServerMsg) -> Result<DeleteOutcome, NetError> {
    exchange(BUDGET, |mut srd, mut swr| async move {
        let _request: ClientMsg = read_msg(&mut srd).await.expect("a request");
        write_msg(&mut swr, &answer).await.expect("the answer");
    })
    .await
    .0
}

fn reject(code: RejectCode, reason: &str) -> ServerMsg {
    ServerMsg::reject(code, reason)
}

/// Nothing a person reads may carry a control character or text the server chose beyond the bounds.
fn assert_printable(text: &str) {
    assert!(!text.chars().any(char::is_control), "{text:?}");
    assert!(text.chars().count() <= 400, "{} chars", text.chars().count());
}

// ---- the exchange ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_request_is_the_first_and_only_frame_with_the_version_the_build_and_the_token() {
    let (result, (first, after)) = exchange(BUDGET, |mut srd, mut swr| async move {
        let first: ClientMsg = read_msg(&mut srd).await.expect("a first message");
        write_msg(&mut swr, &ServerMsg::CharacterDeleted { name: "Aria".into() }).await.expect("the answer");
        // The client is done and hangs up: a clean end of the pipe, with nothing else sent before it.
        let after = read_msg::<_, ClientMsg>(&mut srd).await;
        (first, after)
    })
    .await;
    assert_eq!(first, expected_request(), "exactly DeleteCharacter, as the first frame, instead of Hello");
    assert!(matches!(first, ClientMsg::DeleteCharacter { protocol_version, .. } if protocol_version == PROTOCOL_VERSION));
    assert!(matches!(after, Err(ProtoError::Closed)), "nothing followed the request: {after:?}");
    assert_eq!(result.expect("deleted"), DeleteOutcome::Deleted("Aria".into()));
}

#[tokio::test]
async fn character_deleted_is_deleted_and_its_name_is_cleaned_like_all_text_from_the_network() {
    let name = answered(ServerMsg::CharacterDeleted { name: "Ar\u{1b}[2Jia\u{202e}\n".into() }).await;
    assert_eq!(name.expect("deleted"), DeleteOutcome::Deleted("Ar[2Jia".into()), "controls and bidirectional overrides are dropped");
    let long = answered(ServerMsg::CharacterDeleted { name: "n".repeat(500) }).await.expect("deleted");
    assert_eq!(long, DeleteOutcome::Deleted("n".repeat(200)), "and the length is bounded");
    let empty = answered(ServerMsg::CharacterDeleted { name: String::new() }).await.expect("deleted");
    assert_eq!(empty, DeleteOutcome::Deleted(String::new()), "a name that is not there is still a deleted character");
}

#[tokio::test]
async fn invalid_token_means_the_character_is_already_gone_and_only_its_code_says_so() {
    for words in ["invalid or unknown token", "we lost your account", ""] {
        let outcome = answered(reject(RejectCode::InvalidToken, words)).await;
        assert_eq!(outcome.expect("already gone"), DeleteOutcome::AlreadyGone, "{words:?}");
    }
    // The same sentence with another code is a refusal like any other: the words are never read for a clue.
    for code in [RejectCode::Unspecified, RejectCode::BadRequest, RejectCode::Internal] {
        let outcome = answered(reject(code, "invalid or unknown token")).await;
        assert!(matches!(outcome, Err(NetError::Rejected { code: got, .. }) if got == code), "{code:?}: {outcome:?}");
    }
}

#[tokio::test]
async fn already_online_stays_an_error_that_keeps_its_code() {
    let error = answered(reject(RejectCode::AlreadyOnline, "that character is in the game right now")).await.expect_err("nothing was deleted");
    match &error {
        NetError::Rejected { code, reason } => {
            assert_eq!(*code, RejectCode::AlreadyOnline);
            assert_eq!(reason, "that character is in the game right now");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(error.exit_code(), exit::FAILURE);
}

#[tokio::test]
async fn protocol_mismatch_stays_an_error_that_keeps_its_code() {
    let error = answered(reject(RejectCode::ProtocolMismatch, "this server speaks v99")).await.expect_err("nothing was deleted");
    assert!(matches!(error, NetError::Rejected { code: RejectCode::ProtocolMismatch, .. }), "{error:?}");
}

#[tokio::test]
async fn every_other_refusal_is_an_error_with_its_code_and_the_words_cleaned() {
    for code in RejectCode::ALL.into_iter().filter(|code| *code != RejectCode::InvalidToken) {
        let error = answered(reject(code, "words\u{1b}[31m of the \u{202e}server\n")).await.expect_err("nothing was deleted");
        match error {
            NetError::Rejected { code: got, reason } => {
                assert_eq!(got, code);
                assert_eq!(reason, "words[31m of the server", "{code:?}: cleaned like the handshake's refusals");
            }
            other => panic!("{code:?}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_server_that_closes_without_answering_is_a_connection_error() {
    let (result, _) = exchange(BUDGET, |mut srd, swr| async move {
        let _request: ClientMsg = read_msg(&mut srd).await.expect("a request");
        drop(swr);
        drop(srd);
    })
    .await;
    let error = result.expect_err("no answer");
    match &error {
        NetError::Io { during, detail } => {
            assert!(during.contains("CharacterDeleted"), "{during}");
            assert!(detail.contains("closed the connection"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert_printable(&error.to_string());
}

#[tokio::test]
async fn a_server_that_is_gone_before_the_request_is_a_connection_error() {
    let (result, ()) = exchange(BUDGET, |srd, swr| async move {
        drop(swr);
        drop(srd);
    })
    .await;
    assert!(matches!(result, Err(NetError::Io { .. })), "{result:?}");
}

/// A server that holds the connection open and says nothing (a stopped process, a black-holing network): the one
/// deadline ends the wait. On tokio's paused clock, so it is exact and independent of the load of the machine.
#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_runs_into_the_deadline() {
    let (client, peer) = duplex(64 * 1024);
    let (mut crd, mut cwr) = split(client);
    let (mut srd, swr) = split(peer);
    let started = Instant::now();
    // The server reads the request and then holds the connection open, saying nothing (`swr` stays alive).
    let server = tokio::spawn(async move {
        let _request: ClientMsg = read_msg(&mut srd).await.expect("a request");
        tokio::time::sleep(Duration::from_secs(600)).await;
    });
    let result = delete_character(&mut crd, &mut cwr, &token(), "test-build", Duration::from_secs(3)).await;
    let waited = started.elapsed();
    assert!(matches!(result, Err(NetError::Timeout("CharacterDeleted"))), "{result:?}");
    assert!(waited >= Duration::from_secs(3) && waited < Duration::from_secs(10), "gave up after {waited:?}");
    assert!(result.unwrap_err().to_string().contains("timed out"));
    server.abort();
    drop(swr);
}

#[tokio::test]
async fn something_that_does_not_belong_is_a_protocol_error_that_echoes_nothing_of_the_server() {
    let welcome = ServerMsg::Welcome {
        protocol_version: PROTOCOL_VERSION,
        server_build: "PWNED-build".into(),
        session: fmv_core::SessionId(1),
        entity: EntityId(7),
        account: fmv_core::AccountId(42),
        issued_token: Some(Secret::new("fmvt_issued_by_nobody")),
        character: fmv_protocol::CharacterCard { name: "PWNED".into(), tint: [1, 2, 3], body: None, binding: None, slot: None, migrated: false },
    };
    for message in [ServerMsg::Pong { nonce: 1, tick: 1 }, ServerMsg::PlayerLeft { entity: EntityId(3) }, welcome] {
        let error = answered(message).await.expect_err("not an answer to a delete");
        assert!(matches!(error, NetError::Protocol(_)), "{error:?}");
        let text = error.to_string();
        assert_printable(&text);
        assert!(!text.contains("PWNED") && !text.contains("fmvt_issued"), "{text}");
    }
}

#[tokio::test]
async fn an_answer_that_cannot_be_decoded_is_a_clean_error() {
    // A frame whose JSON object has one hostile variant name: serde's error would echo it.
    let name = format!("CharacterDeleted\\u001b]0;PWNED-TITLE\\u0007\\u001b[2J\\nforged {}", "x".repeat(5000));
    let payload = format!("{{\"{name}\":{{}}}}").into_bytes();
    let mut frame = u32::try_from(payload.len()).expect("a small frame").to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    let (result, ()) = exchange(BUDGET, |mut srd, mut swr| async move {
        let _request: ClientMsg = read_msg(&mut srd).await.expect("a request");
        swr.write_all(&frame).await.expect("the hostile frame");
        swr.flush().await.expect("flushed");
    })
    .await;
    let error = result.expect_err("not an answer");
    assert!(matches!(error, NetError::Io { .. }), "{error:?}");
    let text = error.to_string();
    assert_printable(&text);
    assert!(!text.contains("PWNED") && !text.contains("forged") && !text.contains("xxxx"), "{text}");
}

#[tokio::test]
async fn the_token_is_in_the_request_and_nowhere_else() {
    let outcomes = [
        answered(ServerMsg::CharacterDeleted { name: "Aria".into() }).await,
        answered(reject(RejectCode::InvalidToken, "x")).await,
        answered(reject(RejectCode::AlreadyOnline, "x")).await,
        answered(ServerMsg::Pong { nonce: 1, tick: 1 }).await,
    ];
    for outcome in outcomes {
        let shown = match &outcome {
            Ok(o) => format!("{o:?}"),
            Err(e) => format!("{e} {e:?}"),
        };
        assert!(!shown.contains(TOKEN), "{shown}");
    }
    // The request itself holds it (that is what it is for), but its Debug never prints it.
    assert!(!format!("{:?}", expected_request()).contains(TOKEN));
}

// ---- over a real socket ----------------------------------------------------------------------------------------------

#[test]
fn over_tcp_the_request_is_the_first_frame_and_the_answer_is_the_outcome() {
    let (addr, server) = serve_delete(DeleteReply::Deleted("Aria"));
    let outcome = delete_blocking(&addr, &token(), "test-build").expect("deleted");
    assert_eq!(outcome, DeleteOutcome::Deleted("Aria".into()));
    let seen = server.join().expect("the fake server");
    assert_eq!(seen.first, Some(expected_request()), "the first frame of the connection is the request");
    assert_eq!(seen.after, 0, "and nothing followed it");
}

#[test]
fn over_tcp_a_login_the_server_does_not_know_is_already_gone_and_a_character_in_the_game_is_refused() {
    let (addr, server) = serve_delete(DeleteReply::Reject(RejectCode::InvalidToken, "invalid or unknown token"));
    assert_eq!(delete_blocking(&addr, &token(), "test-build").expect("already gone"), DeleteOutcome::AlreadyGone);
    assert_eq!(server.join().expect("the fake server").first, Some(expected_request()));

    let (addr, server) = serve_delete(DeleteReply::Reject(RejectCode::AlreadyOnline, "that character is in the game right now"));
    let error = delete_blocking(&addr, &token(), "test-build").expect_err("refused");
    assert!(matches!(error, NetError::Rejected { code: RejectCode::AlreadyOnline, .. }), "{error:?}");
    assert_eq!(server.join().expect("the fake server").first, Some(expected_request()));
}

#[test]
fn over_tcp_a_server_that_hangs_up_or_talks_nonsense_is_an_error() {
    let (addr, server) = serve_delete(DeleteReply::Hangup);
    let error = delete_blocking(&addr, &token(), "test-build").expect_err("no answer");
    assert!(matches!(error, NetError::Io { .. }), "{error:?}");
    assert_eq!(server.join().expect("the fake server").first, Some(expected_request()));

    let (addr, server) = serve_delete(DeleteReply::Nonsense);
    let error = delete_blocking(&addr, &token(), "test-build").expect_err("not an answer");
    assert!(matches!(error, NetError::Protocol(_)), "{error:?}");
    drop(server.join());
}

#[test]
fn a_refused_connection_is_a_connect_error() {
    // Nothing listens on this port any more.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    let error = delete_blocking(&addr, &token(), "test-build").expect_err("nobody home");
    assert!(matches!(&error, NetError::Connect { addr: a, .. } if *a == addr), "{error:?}");
    assert!(error.to_string().contains("is the server running"), "{error}");
}

// ---- what a failure says ---------------------------------------------------------------------------------------------

#[test]
fn a_failure_says_which_character_was_not_deleted_and_why() {
    let refused = NetError::Rejected { code: RejectCode::RateLimited, reason: "a delete of it is under way".into() };
    assert_eq!(failure_text("Aria", &refused), "Aria was NOT deleted and its saved login was kept. The server refused: a delete of it is under way");
    let online = NetError::Rejected { code: RejectCode::AlreadyOnline, reason: "words nobody reads".into() };
    assert_eq!(failure_text("Aria", &online), "Aria is in the game right now (from this or another computer).");
    let refused_connection = NetError::Connect { addr: "127.0.0.1:1".into(), source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused") };
    let text = failure_text("Aria", &refused_connection);
    assert!(text.starts_with("Aria was NOT deleted and its saved login was kept: cannot connect to 127.0.0.1:1"), "{text}");
    assert!(failure_text("Aria", &NetError::Timeout("CharacterDeleted")).contains("timed out"));
    // The words of a refusal are only quoted: the same words with the AlreadyOnline code are the player's own sentence.
    let same_words = NetError::Rejected { code: RejectCode::Internal, reason: "words nobody reads".into() };
    assert_ne!(failure_text("Aria", &same_words), failure_text("Aria", &online));
}

// ---- the --delete command ---------------------------------------------------------------------------------------------

const SERVER: &str = "127.0.0.1:47770";

struct World {
    _dir: tempfile::TempDir,
    store: TokenStore,
    looks: LookCache,
}

/// A temporary state directory with the logins of `names` saved for [`SERVER`] (token `tok_<lowercase name>`) and a note
/// about each one's look.
fn world(names: &[&str]) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TokenStore::new(dir.path());
    let looks = LookCache::new(dir.path());
    for name in names {
        store.save_new(SERVER, name, &Secret::new(format!("tok_{}", name.to_lowercase().replace(' ', "_")))).expect("save");
        looks.store(SERVER, name, &Look::plain([1, 2, 3], None)).expect("a note");
    }
    World { _dir: dir, store, looks }
}

/// Run `--delete name` against `world`, with `ask` as the network. Returns the exit code, stdout and stderr.
fn command(world: &World, name: &str, ask: impl FnOnce(&Secret) -> Result<DeleteOutcome, NetError>) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let cmd = Command { server: SERVER, name, store: &world.store, looks: &world.looks };
    let code = run_command(&cmd, ask, &mut out, &mut err);
    (code, String::from_utf8(out).expect("utf-8"), String::from_utf8(err).expect("utf-8"))
}

fn never_asked(_: &Secret) -> Result<DeleteOutcome, NetError> {
    panic!("the server must not be asked")
}

fn assert_no_token(out: &str, err: &str) {
    for text in [out, err] {
        assert!(!text.contains("tok_"), "a token was printed: {text}");
    }
}

fn saved_names(world: &World) -> Vec<String> {
    world.store.list(SERVER).accounts.into_iter().map(|a| a.name).collect()
}

#[test]
fn the_command_deletes_with_the_login_of_that_name_and_removes_only_that_login_and_its_note() {
    // Cloud sits in the server's default file, Tifa in a file of her own: either way only the one asked for goes.
    for (victim, other) in [("Tifa", "Cloud"), ("Cloud", "Tifa")] {
        let w = world(&["Cloud", "Tifa"]);
        let wanted = format!("tok_{}", victim.to_lowercase());
        let (code, out, err) = command(&w, victim, |token| {
            assert_eq!(token.expose(), wanted, "the token of the character asked for, and no other");
            Ok(DeleteOutcome::Deleted(victim.to_string()))
        });
        assert_eq!(code, exit::OK, "{err}");
        assert_eq!(out, format!("deleted character name={victim}\n"), "exactly one line on stdout");
        assert_eq!(saved_names(&w), [other], "only {victim}'s login was removed");
        assert_eq!(w.store.find(SERVER, Some(other)).unwrap().unwrap().token.expose(), format!("tok_{}", other.to_lowercase()), "the other login is untouched");
        assert_eq!(w.looks.load(SERVER, victim), None, "its note is gone");
        assert!(w.looks.load(SERVER, other).is_some(), "the other note is untouched");
        assert_no_token(&out, &err);
    }
}

#[test]
fn the_name_is_matched_like_name_and_printed_as_the_saved_login_spells_it() {
    let w = world(&["Cloud", "Tifa"]);
    let (code, out, _) = command(&w, "tIFA", |_| Ok(DeleteOutcome::Deleted("Tifa".into())));
    assert_eq!(code, exit::OK);
    assert_eq!(out, "deleted character name=Tifa\n", "the saved login's own spelling, not the one typed");
}

#[test]
fn a_character_the_server_no_longer_has_is_forgotten_here_and_says_so_in_its_own_line() {
    let w = world(&["Cloud", "Tifa"]);
    let (code, out, err) = command(&w, "Cloud", |_| Ok(DeleteOutcome::AlreadyGone));
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "character already gone on the server name=Cloud\n");
    assert_eq!(saved_names(&w), ["Tifa"]);
    assert_eq!(w.looks.load(SERVER, "Cloud"), None);
}

#[test]
fn the_name_in_the_line_is_percent_encoded_so_the_line_splits_on_spaces() {
    let w = world(&["Red XIII"]);
    let (code, out, _) = command(&w, "Red XIII", |_| Ok(DeleteOutcome::Deleted("Red XIII".into())));
    assert_eq!(code, exit::OK);
    assert_eq!(out, "deleted character name=Red%20XIII\n");
    assert_eq!(out.split(' ').count(), 3, "deleted, character, name=...");
}

#[test]
fn a_name_with_no_saved_login_is_exit_two_and_nothing_is_asked() {
    let w = world(&["Cloud"]);
    let (code, out, err) = command(&w, "Tifa", never_asked);
    assert_eq!(code, exit::USAGE);
    assert_eq!(out, "", "nothing on stdout");
    assert!(err.contains("no login is saved for \"Tifa\"") && err.contains(SERVER), "{err}");
    assert_eq!(saved_names(&w), ["Cloud"], "nothing was touched");
    // No saved login at all: the same.
    let nobody = world(&[]);
    assert_eq!(command(&nobody, "Cloud", never_asked).0, exit::USAGE);
}

#[test]
fn a_failure_is_exit_one_with_the_error_on_stderr_and_the_login_kept() {
    let failures = [
        NetError::Connect { addr: SERVER.into(), source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused") },
        NetError::ConnectTimeout(SERVER.into(), 5),
        NetError::Timeout("CharacterDeleted"),
        NetError::Io { during: "waiting for CharacterDeleted", detail: "the server closed the connection".into() },
        NetError::Protocol("the server answered the delete request with something else".into()),
        NetError::Rejected { code: RejectCode::ProtocolMismatch, reason: "this server speaks v99".into() },
        NetError::Rejected { code: RejectCode::RateLimited, reason: "a delete of it is under way".into() },
        NetError::Rejected { code: RejectCode::Internal, reason: "internal error".into() },
    ];
    for failure in failures {
        let w = world(&["Cloud", "Tifa"]);
        let (code, out, err) = command(&w, "Cloud", |_| Err(failure));
        assert_eq!(code, exit::FAILURE, "{err}");
        assert_eq!(out, "", "no line on stdout for a failure");
        assert!(err.contains("error: Cloud was NOT deleted and its saved login was kept"), "{err}");
        assert_eq!(saved_names(&w), ["Cloud", "Tifa"], "both logins are kept");
        assert!(w.looks.load(SERVER, "Cloud").is_some(), "and the note");
        assert_no_token(&out, &err);
    }
}

#[test]
fn a_character_that_is_in_the_game_is_refused_with_its_own_sentence() {
    let w = world(&["Cloud"]);
    let online = NetError::Rejected { code: RejectCode::AlreadyOnline, reason: "that character is in the game right now".into() };
    let (code, out, err) = command(&w, "Cloud", |_| Err(online));
    assert_eq!(code, exit::FAILURE);
    assert_eq!(out, "");
    assert!(err.contains("Cloud is in the game right now (from this or another computer)."), "{err}");
    assert!(err.contains("Leave the game first, then run --delete again."), "{err}");
    assert_eq!(saved_names(&w), ["Cloud"]);
}

#[test]
fn every_refusal_but_invalid_token_keeps_the_login() {
    for code in RejectCode::ALL.into_iter().filter(|code| *code != RejectCode::InvalidToken) {
        let w = world(&["Cloud"]);
        let (exit_code, out, err) = command(&w, "Cloud", |_| Err(NetError::Rejected { code, reason: "no".into() }));
        assert_eq!((exit_code, out.as_str()), (exit::FAILURE, ""), "{code:?}: {err}");
        assert_eq!(saved_names(&w), ["Cloud"], "{code:?}");
    }
}

#[test]
fn a_login_file_that_cannot_be_used_is_exit_one_before_anything_is_asked() {
    let w = world(&["Cloud"]);
    let path = w.store.list(SERVER).accounts[0].path.clone();
    std::fs::write(&path, "tok_without_a_name_line\n").expect("break the file");
    let (code, out, err) = command(&w, "Cloud", never_asked);
    assert_eq!((code, out.as_str()), (exit::FAILURE, ""));
    assert!(err.contains("unusable"), "{err}");
    assert!(path.exists(), "the file is left for the player to fix");
}

#[test]
fn a_login_that_changed_during_the_delete_is_not_removed_and_the_run_fails() {
    let w = world(&["Cloud"]);
    let store = w.store.clone();
    let (code, out, err) = command(&w, "Cloud", |_| {
        // Another process registered the name anew while the server was deleting the old character.
        store.save_replacing(SERVER, "Cloud", &Secret::new("tok_the_new_one")).expect("replace");
        Ok(DeleteOutcome::Deleted("Cloud".into()))
    });
    assert_eq!(code, exit::FAILURE, "the saved login could not be removed");
    assert_eq!(out, "deleted character name=Cloud\n", "the server's side is done and the line says so");
    assert!(err.contains("could not be removed") && err.contains("changed since it was listed"), "{err}");
    assert_eq!(w.store.find(SERVER, Some("Cloud")).unwrap().unwrap().token.expose(), "tok_the_new_one", "the new login is untouched");
    assert!(!err.contains("tok_"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_token_file_that_others_can_read_is_warned_about_and_still_used() {
    use std::os::unix::fs::PermissionsExt;
    let w = world(&["Cloud"]);
    let path = w.store.list(SERVER).accounts[0].path.clone();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let (code, out, err) = command(&w, "Cloud", |_| Ok(DeleteOutcome::Deleted("Cloud".into())));
    assert_eq!(code, exit::OK, "{err}");
    assert!(err.contains("readable by other users"), "{err}");
    assert_eq!(out, "deleted character name=Cloud\n");
    assert_no_token(&out, &err);
}
