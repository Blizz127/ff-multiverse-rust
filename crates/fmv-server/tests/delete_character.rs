//! Deleting a character over a real socket: only the one who holds the token can ask, it is refused while the
//! character is in the world, the name is free afterwards, nothing of anybody else changes, and a restart brings
//! nothing back. Every server here keeps its accounts in a temporary state directory.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use fmv_protocol::{ClientMsg, RejectCode, Secret, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ServerConfig;

fn config(state: &Path) -> ServerConfig {
    // Registrations are not what is under test: lift the per-address limit that a loop of them would hit.
    ServerConfig { state_dir: Some(state.to_path_buf()), registrations_per_ip: None, ..ServerConfig::default() }
}

fn delete_msg(token: &str) -> ClientMsg {
    ClientMsg::DeleteCharacter { protocol_version: PROTOCOL_VERSION, client_build: "integration-test".into(), token: Secret::new(token) }
}

/// Ask for a delete on a connection of its own: the name that was freed, or the refusal. The connection is closed by
/// the server after either answer.
async fn delete(addr: SocketAddr, token: &str) -> Result<String, (RejectCode, String)> {
    delete_msg_on(Client::connect(addr).await, delete_msg(token)).await
}

async fn delete_msg_on(mut client: Client, msg: ClientMsg) -> Result<String, (RejectCode, String)> {
    client.send(&msg).await;
    let answer = match client.next().await {
        ServerMsg::CharacterDeleted { name } => Ok(name),
        ServerMsg::Reject { code, reason } => Err((code, reason)),
        other => panic!("a delete is answered with CharacterDeleted or a Reject, got {other:?}"),
    };
    client.expect_closed().await;
    answer
}

/// Every file of the state directory, relative path to bytes (`LOCK` left out).
fn file_map(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel != "LOCK" {
                    out.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The record files of one account, as the store files them (the avatar id is looked up in `avatars/`).
fn records_of(root: &Path, account: u64) -> BTreeMap<String, Vec<u8>> {
    let all = file_map(root);
    let avatar_files: Vec<String> = all
        .iter()
        .filter(|(rel, bytes)| {
            rel.starts_with("avatars/") && serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v| v["account_id"] == account)
        })
        .map(|(rel, _)| rel.clone())
        .collect();
    all.into_iter()
        .filter(|(rel, _)| {
            rel == &format!("accounts/{account}.json")
                || rel == &format!("characters/{account}.json")
                || rel.starts_with(&format!("progression/{account}/"))
                || avatar_files.contains(rel)
        })
        .collect()
}

/// The player leaves; once the server has closed the connection it has also taken the player out of the world (it does
/// that before it closes), so the character can be deleted.
async fn leave(mut joined: Joined) {
    joined.client.send(&ClientMsg::Leave).await;
    joined.client.expect_closed().await;
}

fn entries_in_trash(state: &Path) -> Vec<String> {
    match fs::read_dir(state.join("trash")) {
        Ok(dir) => {
            let mut names: Vec<String> = dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

#[tokio::test]
async fn only_the_holder_of_the_token_can_delete_a_character_and_only_that_one_goes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(config(&state)).await;
    let (aria_name, brom_name) = (uniq("Aria"), uniq("Brom"));
    let aria = join(server.addr, &aria_name).await;
    let brom = join(server.addr, &brom_name).await;
    let (aria_account, brom_account, aria_token, brom_token) = (aria.account.0, brom.account.0, aria.token.clone(), brom.token.clone());
    leave(aria).await;
    leave(brom).await;
    let brom_before = records_of(&state, brom_account);
    assert!(brom_before.len() >= 3, "{:?}", brom_before.keys());
    let aria_before = records_of(&state, aria_account);

    // A stranger without the token cannot: nothing is deleted, and the answer is the one for an unknown login.
    for wrong in ["", "garbage", "fmvt_0000000000000000000000000000000000000000000000000000000000000000", &format!("{aria_token}x")] {
        let (code, _) = delete(server.addr, wrong).await.unwrap_err();
        assert_eq!(code, RejectCode::InvalidToken, "{wrong:?}");
    }
    assert_eq!(records_of(&state, aria_account), aria_before, "nothing of Aria changed");
    assert!(entries_in_trash(&state).is_empty());

    // Aria's own token deletes Aria's character and no other.
    let name = delete(server.addr, &aria_token).await.unwrap();
    assert_eq!(name, aria_name);
    assert!(records_of(&state, aria_account).is_empty(), "her records are out of the live directories");
    let trash = entries_in_trash(&state);
    assert_eq!(trash.len(), 1, "{trash:?}");
    assert!(trash[0].ends_with(&format!("-{aria_account}")), "{trash:?}");
    for (rel, bytes) in &aria_before {
        if rel.ends_with(".location.json") {
            // Where she last stood (MMO-style saving) is forgotten, not kept: a restored character joins where everyone does.
            assert!(!state.join("trash").join(&trash[0]).join(rel).exists(), "{rel} is not kept in the trash");
            continue;
        }
        assert_eq!(&fs::read(state.join("trash").join(&trash[0]).join(rel)).unwrap(), bytes, "{rel} is in the trash as it was");
    }
    assert!(aria_before.keys().any(|rel| rel.ends_with(".location.json")), "her spot was saved when she left: {:?}", aria_before.keys());
    assert_eq!(records_of(&state, brom_account), brom_before, "Brom's records are byte for byte what they were");

    // Her login is gone, the second delete is the answer for an unknown login (the client reads it as "already gone")...
    let mut stale = Client::connect(server.addr).await;
    stale.hello_token(&aria_token).await;
    stale.expect_reject_as(RejectCode::InvalidToken, "invalid or unknown token").await;
    let (code, _) = delete(server.addr, &aria_token).await.unwrap_err();
    assert_eq!(code, RejectCode::InvalidToken);
    assert_eq!(entries_in_trash(&state).len(), 1, "and nothing more was moved");

    // ...her name is free again (the new character is a different account with a different token)...
    let again = join(server.addr, &aria_name).await;
    assert_ne!(again.token, aria_token);
    assert!(again.account.0 > brom_account, "ids are never reused");
    leave(again).await;
    // ...and Brom plays on with his own token.
    let brom_again = join_with_token(server.addr, &brom_token).await;
    assert_eq!(brom_again.account.0, brom_account);
    leave(brom_again).await;
    assert_eq!(records_of(&state, brom_account), brom_before, "playing did not rewrite his records either");
    server.shutdown().await;
}

#[tokio::test]
async fn a_character_in_the_world_is_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(config(&state)).await;
    let name = uniq("Online");
    let mut aria = join(server.addr, &name).await;
    let before = file_map(&state);

    // Asked from another computer while the character plays: refused, and nothing is touched.
    let (code, reason) = delete(server.addr, &aria.token).await.unwrap_err();
    assert_eq!(code, RejectCode::AlreadyOnline, "{reason}");
    assert!(reason.contains("in the game"), "{reason}");
    assert!(entries_in_trash(&state).is_empty());
    let mut after = file_map(&state);
    let mut expected = before;
    for map in [&mut after, &mut expected] {
        map.remove("index.log"); // (a first login records itself there; the records are what matter)
    }
    assert_eq!(after, expected, "no file changed");

    // The player is still in the game, untouched.
    aria.client.send(&ClientMsg::Ping { nonce: 77 }).await;
    aria.client.until("Pong", |m| matches!(m, ServerMsg::Pong { nonce: 77, .. }).then_some(())).await;

    // Once it has left, the same request goes through.
    let token = aria.token.clone();
    leave(aria).await;
    assert_eq!(delete(server.addr, &token).await.unwrap(), name);
    assert_eq!(entries_in_trash(&state).len(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn a_login_that_has_not_joined_yet_cannot_join_once_its_character_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(config(&dir.path().join("server-state"))).await;
    let name = uniq("Racer");
    let first = join(server.addr, &name).await;
    let token = first.token.clone();
    leave(first).await;

    // A second login passes authentication and is offered the area, but has not joined the world yet...
    let mut late = Client::connect(server.addr).await;
    late.hello_token(&token).await;
    assert!(matches!(late.next().await, ServerMsg::Welcome { .. }));
    assert!(matches!(late.next().await, ServerMsg::AreaManifest { .. }));
    // ...so the character is not in the world and the delete goes through...
    assert_eq!(delete(server.addr, &token).await.unwrap(), name);
    // ...and the late login is not let in as a character that no longer exists.
    late.send(&join_area()).await;
    late.expect_reject_as(RejectCode::InvalidToken, "invalid or unknown token").await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_delete_of_another_protocol_version_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(config(&state)).await;
    let name = uniq("Old");
    let first = join(server.addr, &name).await;
    let (token, account) = (first.token.clone(), first.account.0);
    leave(first).await;
    let before = records_of(&state, account);
    for version in [0, PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1, u16::MAX] {
        let msg = ClientMsg::DeleteCharacter { protocol_version: version, client_build: "old".into(), token: Secret::new(token.clone()) };
        let (code, reason) = delete_msg_on(Client::connect(server.addr).await, msg).await.unwrap_err();
        assert_eq!(code, RejectCode::ProtocolMismatch, "{reason}");
        assert!(reason.contains("protocol version mismatch"), "{reason}");
    }
    assert_eq!(records_of(&state, account), before);
    assert!(entries_in_trash(&state).is_empty());
    let again = join_with_token(server.addr, &token).await;
    assert_eq!(again.account.0, account, "the character is still there");
    leave(again).await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_delete_is_only_ever_the_first_message_of_a_connection() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(config(&state)).await;
    let name = uniq("Sneaky");
    let mut player = join(server.addr, &name).await;
    let token = player.token.clone();

    // In the game: refused like any message out of place, and the character stays.
    player.client.send(&delete_msg(&token)).await;
    let (code, reason) = player.client.next_reject().await;
    assert_eq!(code, RejectCode::BadRequest, "{reason}");
    assert!(reason.contains("unexpected message after joining"), "{reason}");
    player.client.expect_closed().await;

    // Between Hello and JoinArea: the same.
    let mut half = Client::connect(server.addr).await;
    half.hello_token(&token).await;
    assert!(matches!(half.next().await, ServerMsg::Welcome { .. }));
    half.send(&delete_msg(&token)).await;
    half.expect_reject_as(RejectCode::BadRequest, "unexpected message before JoinArea").await;

    assert!(entries_in_trash(&state).is_empty(), "nothing was deleted");
    let again = join_with_token(server.addr, &token).await;
    leave(again).await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_deleted_character_stays_deleted_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let (aria_name, brom_name) = (uniq("Aria"), uniq("Brom"));

    let server = TestServer::start(config(&state)).await;
    let aria = join(server.addr, &aria_name).await;
    let brom = join(server.addr, &brom_name).await;
    let (aria_token, brom_token, brom_account) = (aria.token.clone(), brom.token.clone(), brom.account.0);
    leave(aria).await;
    leave(brom).await;
    assert_eq!(delete(server.addr, &aria_token).await.unwrap(), aria_name);
    let brom_records = records_of(&state, brom_account);
    server.shutdown().await;

    // A new server process on the same state directory: nothing comes back.
    let server = TestServer::start(config(&state)).await;
    let mut stale = Client::connect(server.addr).await;
    stale.hello_token(&aria_token).await;
    stale.expect_reject_as(RejectCode::InvalidToken, "invalid or unknown token").await;
    assert_eq!(entries_in_trash(&state).len(), 1, "the trash is still there");
    let brom_again = join_with_token(server.addr, &brom_token).await;
    leave(brom_again).await;
    // The name is free after the restart too.
    let again = join(server.addr, &aria_name).await;
    assert_ne!(again.token, aria_token);
    leave(again).await;
    assert_eq!(records_of(&state, brom_account), brom_records, "Brom's records are still what they were");
    server.shutdown().await;
}

#[tokio::test]
async fn the_answer_to_a_delete_always_arrives_before_the_connection_closes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(config(&state)).await;
    // Many in a row: a reply that the close could destroy in flight would show up as a hang or a missing answer.
    for round in 0..40 {
        let name = uniq("Loop");
        let player = join(server.addr, &name).await;
        let token = player.token.clone();
        leave(player).await;
        let answer = tokio::time::timeout(Duration::from_secs(10), delete(server.addr, &token)).await.unwrap_or_else(|_| panic!("round {round}: no answer"));
        assert_eq!(answer.as_deref(), Ok(name.as_str()), "round {round}");
    }
    assert_eq!(entries_in_trash(&state).len(), 40);
    server.shutdown().await;
}

#[tokio::test]
async fn two_deletes_of_one_character_at_once_delete_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(config(&state)).await;
    let name = uniq("Twice");
    let player = join(server.addr, &name).await;
    let token = player.token.clone();
    leave(player).await;
    let (addr, a, b) = (server.addr, token.clone(), token);
    let (first, second) = tokio::join!(delete(addr, &a), delete(addr, &b));
    // One of them did it. The other is told the character is gone (it asked after) or that a delete is under way (it
    // asked during): never that something else is wrong, and never that nothing happened and the login is fine.
    let results = [first, second];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{results:?}");
    for refused in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(matches!(refused.0, RejectCode::InvalidToken | RejectCode::RateLimited), "{refused:?}");
    }
    assert_eq!(entries_in_trash(&state).len(), 1);
    server.shutdown().await;
}

/// (keeps the helper type used: a path in the trash, for the failure messages above)
#[allow(dead_code)]
fn trash_path(state: &Path, entry: &str) -> PathBuf {
    state.join("trash").join(entry)
}
