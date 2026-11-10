//! The original flower encounter, reached by walking its real predecessor gateway.
//! Reads owned assets; every account and save belongs to a temporary directory.

mod common;
#[path = "common/walk.rs"]
mod walk;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, TestServer};
use fmv_adapter_api::{AreaRuntime, Exit};
use fmv_core::{AccountId, AnimState, EntityId, GameId, InventoryEntry, Pos};
use fmv_ff7::script::scene::{flower_station_scene, ScenePlay, SceneResources, Then, FLOWER_STATION_SCENE_ID};
use fmv_ff7::story::StoryState;
use fmv_persist::{CharacterProgress, ProgressPair, Progression, Store};
use fmv_protocol::{Auth, ClientMsg, NewCharacter, SceneOutcome, SceneRole, Secret, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ff7::{open_battle_data, open_ff7, prewarm, Ff7Source, SharedFf7};
use fmv_server::scene_host::Ff7SceneHost;
use fmv_server::{BattleSettings, SceneSettings, ServerConfig};

struct Case {
    account: AccountId,
    token: String,
    answers: Vec<usize>,
    before: Vec<u8>,
    gil: u32,
    bought: bool,
}

struct Malformed {
    account: AccountId,
    token: String,
    answers: Vec<u8>,
    skipped: bool,
    before: ProgressPair,
    bytes: [Vec<u8>; 2],
}

struct Arrived {
    entity: EntityId,
    pos: Pos,
    gil: u32,
    progress: Vec<u8>,
}

/// Answer every normal area handshake, including the story's relogin placement.
async fn login(client: &mut Client, token: &str, target: &str) -> Arrived {
    client.send(&ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, client_build: "flower-route-test".into(),
        auth: Auth::Token(Secret::new(token.to_string())), character: NewCharacter::with_tint([9, 9, 9]) }).await;
    let mut entity = None;
    let mut gil = None;
    let mut progress = None;
    loop {
        match client.next().await {
            ServerMsg::Welcome { entity: e, .. } => entity = Some(e),
            ServerMsg::CharacterProgress { gil: value, .. } => gil = Some(value),
            ServerMsg::StoryUpdate { progress: value, .. } => progress = Some(value),
            ServerMsg::AreaManifest { area, fingerprint, provenance, .. }
            | ServerMsg::AreaChange { area, fingerprint, provenance, .. } => {
                client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
            }
            ServerMsg::AreaSnapshot { area, entities, .. } if area.key == target => {
                let entity = entity.expect("Welcome before joining");
                let pos = entities.iter().find(|p| p.id == entity).expect("own entity in snapshot").pos;
                // Initial progress can follow the first snapshot, but precedes a subsequent story entry.
                if let (Some(gil), Some(progress)) = (gil, progress.clone()) { return Arrived { entity, pos, gil, progress }; }
            }
            ServerMsg::SceneStart { .. } => panic!("a completed flower scene must not replay on login"),
            ServerMsg::Reject { code, reason } => panic!("login refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

async fn walk_to_flower(client: &mut Client, station: &dyn AreaRuntime, from: Pos, exit: &Exit, flower: &dyn AreaRuntime) {
    let path = walk::route_across(station, from, exit).expect("walkable predecessor gateway");
    for (n, &pos) in path.iter().enumerate() {
        tokio::time::sleep(Duration::from_millis(100)).await;
        client.send(&ClientMsg::MoveIntent { seq: n as u32 + 1, pos, facing: 0.0, anim: AnimState::Walk }).await;
    }
    let (area, fingerprint, provenance, arrival) = client.until("flower gateway offer", |m| match m {
        ServerMsg::AreaChange { area, fingerprint, provenance, arrival, .. } => Some((area.clone(), *fingerprint, provenance.clone(), *arrival)),
        ServerMsg::MoveRejected { seq, corrected } => panic!("route step {seq} refused: {corrected:?}"),
        _ => None,
    }).await;
    assert_eq!(area.key, "md8_2");
    assert_eq!(arrival, flower.place_arrival(exit.arrival, exit.arrival_hint));
    client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
}

fn progress_bytes(state: &Path, account: AccountId) -> [Vec<u8>; 2] {
    let dir = state.join("progression").join(account.0.to_string());
    [std::fs::read(dir.join("Ff7.json")).unwrap(), std::fs::read(dir.join("Ff7.character.json")).unwrap()]
}

fn config(state: &Path, adapter: Arc<fmv_ff7::Ff7Adapter>, rules: Arc<fmv_protocol::Ff7BattleData>) -> ServerConfig {
    ServerConfig {
        area_key: "md1stin".into(), state_dir: Some(state.to_path_buf()),
        battle: Some(BattleSettings { random_encounters: false, ..BattleSettings::new(rules) }),
        scenes: Some(SceneSettings { host: Arc::new(Ff7SceneHost::with_extended_save(adapter, true)),
            debug_preview: false, debug_new_story: Some(StoryState::at(36).encode()) }),
        ..ServerConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs FMV_FF7_LIBRARY; owned assets are read-only and all state is temporary"]
async fn all_flower_choices_walk_the_real_gateway_save_once_and_survive_server_restart() {
    let library = std::env::var_os("FMV_FF7_LIBRARY").expect("owned library path");
    let source = Ff7Source::Library { state_dir: Some(library.into()), set: None };
    let opened = open_ff7(&source).unwrap();
    prewarm(&opened.adapter).await.unwrap();
    let (rules, _) = open_battle_data(&source).unwrap();
    let station = opened.adapter.load_field("md8_1").unwrap();
    let flower = opened.adapter.load_field("md8_2").unwrap();
    let exit = station.exits().into_iter().find(|e| e.target == "md8_2").expect("authored gateway");
    let state = tempfile::tempdir().unwrap();
    let mut cases = Vec::new();
    let mut malformed = Vec::new();
    {
        let store = Store::open(state.path()).unwrap();
        for gil in [0u32, 1, 600] {
            for (answers, bought) in [(&[2usize][..], false), (&[3, 2, 0][..], true), (&[3, 3, 0][..], true),
                (&[3, 2, 1][..], false), (&[3, 3, 1][..], false)] {
                let (account, token) = store.create_account(&format!("Flower{}", cases.len()), 1).unwrap();
                let story = StoryState::at(36);
                // One purchase starts from an empty debug account. The queued debug baseline
                // must reach the same saver before the scene's exact compare-and-swap.
                let debug_seed = cases.len() == 11;
                let before = if debug_seed || cases.len() % 2 == 0 { story.encode() } else { story.into_extended().unwrap().encode() };
                if !debug_seed {
                    store.put_progression(&Progression::new(account, GameId::Ff7, before.clone(), 1)).unwrap();
                }
                let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 6, 1);
                character.gil = gil;
                character.exp = 42;
                character.ap = 7;
                character.limit = 33;
                character.inventory = Some(vec![InventoryEntry { item: 0, count: 3 }]);
                store.put_character_progress(&character).unwrap();
                cases.push(Case { account, token, answers: answers.to_vec(), before, gil, bought });
            }
        }
        for (answers, skipped) in [(&[][..], false), (&[3][..], false), (&[3, 2][..], false), (&[3, 2, 9][..], false), (&[2, 0][..], false), (&[][..], true)] {
            let (account, token) = store.create_account(&format!("Incomplete{}", malformed.len()), 1).unwrap();
            store.put_progression(&Progression::new(account, GameId::Ff7, StoryState::at(36).encode(), 1)).unwrap();
            let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 6, 1);
            character.gil = 600;
            character.inventory = Some(vec![]);
            store.put_character_progress(&character).unwrap();
            malformed.push(Malformed { account, token, answers: answers.to_vec(), skipped,
                before: store.get_progress_pair(account, GameId::Ff7).unwrap(), bytes: progress_bytes(state.path(), account) });
        }
    }
    let server = TestServer::start_over(config(state.path(), opened.adapter.clone(), rules.clone()), Box::new(SharedFf7(opened.adapter.clone()))).await;
    for (index, case) in cases.iter().enumerate() {
        let mut client = Client::connect(server.addr).await;
        let arrived = login(&mut client, &case.token, "md8_1").await;
        assert_eq!((arrived.gil, arrived.progress), (case.gil, case.before.clone()));
        walk_to_flower(&mut client, &station, arrived.pos, &exit, &flower).await;
        let (run, seed, progress, gil) = client.until("automatic flower encounter", |m| match m {
            ServerMsg::SceneStart { run, seed, scene, progress, gil, owner, you, .. } => {
                assert_eq!(scene, FLOWER_STATION_SCENE_ID);
                assert_eq!((*owner, *you), (arrived.entity, SceneRole::Owner));
                Some((*run, *seed, progress.clone(), *gil))
            }
            ServerMsg::Reject { code, reason } => panic!("scene refused: {code:?}: {reason}"),
            _ => None,
        }).await;
        assert_eq!((&progress, gil), (&case.before, case.gil), "wire snapshot retains exact stored predecessor");
        let mut play = ScenePlay::with_resources(flower.field_file(), &flower_station_scene(), &StoryState::decode(&progress).unwrap(),
            seed, SceneResources { gil: Some(gil) }).unwrap();
        let mut answers = case.answers.iter().copied();
        let mut next = None;
        let mut step = 0;
        loop {
            match play.next(next.take()).then {
                Then::Window(window) => {
                    let answer = window.message.choice.map(|choice| {
                        let answer = answers.next().expect("authored choice");
                        assert!((choice.first..=choice.last).contains(&answer));
                        answer
                    });
                    client.send(&ClientMsg::SceneStep { run, step, answer: answer.map(|a| a as u8) }).await;
                    next = answer;
                    step += 1;
                }
                Then::End { how, .. } => { assert_eq!(how, fmv_ff7::script::dialogue::End::Finished); break; }
            }
        }
        assert!(answers.next().is_none());
        client.send(&ClientMsg::SceneEnd { run, skipped: false }).await;
        client.send(&ClientMsg::SceneEnd { run, skipped: false }).await;
        let mut told_story = false;
        let mut told_gil = false;
        loop {
            match client.next().await {
                ServerMsg::StoryUpdate { progress, advance: Some(_), .. } => {
                    let story = StoryState::decode(&progress).unwrap();
                    assert!(story.supports_extended());
                    assert_eq!(story.moment, 39);
                    assert_eq!(story.known_bit(3, 66, 0), Some(true));
                    assert_eq!(story.known_bit(3, 111, 0), case.bought.then_some(true));
                    told_story = true;
                }
                ServerMsg::CharacterProgress { gil, .. } => { assert_eq!(gil, if case.bought { case.gil.saturating_sub(1) } else { case.gil }); told_gil = true; }
                ServerMsg::SceneEnded { run: ended, outcome } => {
                    assert_eq!((ended, outcome), (run, SceneOutcome::Finished));
                    assert!(told_story && told_gil, "durable results precede release");
                    break;
                }
                ServerMsg::Reject { code, reason } => panic!("completion refused: {code:?}: {reason}"),
                _ => {}
            }
        }
        client.send(&ClientMsg::SceneEnd { run, skipped: false }).await;
        assert!(!client.drain_for(Duration::from_millis(100)).await.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { advance: Some(_), .. })));
        eprintln!("flower route case {index}: {step} windows, committed once");
    }
    // A faulty client must not turn a missing choice into the script's default purchase.
    // Also refuse invalid choices and trailing answers rather than silently dropping them.
    for (index, bad) in malformed.iter().enumerate() {
        let mut client = Client::connect(server.addr).await;
        let arrived = login(&mut client, &bad.token, "md8_1").await;
        walk_to_flower(&mut client, &station, arrived.pos, &exit, &flower).await;
        let run = client.until("flower for malformed transcript", |m| match m {
            ServerMsg::SceneStart { run, .. } => Some(*run), _ => None,
        }).await;
        for (step, &answer) in bad.answers.iter().enumerate() {
            client.send(&ClientMsg::SceneStep { run, step: step as u32, answer: Some(answer) }).await;
        }
        client.send(&ClientMsg::SceneEnd { run, skipped: bad.skipped }).await;
        loop {
            let message = tokio::time::timeout(common::STEP, fmv_protocol::read_msg::<_, ServerMsg>(&mut client.stream))
                .await.expect("incomplete scene must terminate within the deadline");
            match message {
                Err(fmv_protocol::ProtoError::Closed | fmv_protocol::ProtoError::Io(_)) if !bad.skipped => break,
                Err(error) => panic!("unexpected connection failure: {error}"),
                Ok(ServerMsg::Reject { code, .. }) => {
                    assert!(!bad.skipped, "a deliberate Back cancels without a storage error");
                    assert_eq!(code, fmv_protocol::RejectCode::Internal);
                }
                Ok(ServerMsg::SceneEnded { run: ended, outcome: SceneOutcome::Aborted }) if bad.skipped => {
                    assert_eq!(ended, run); break;
                }
                Ok(message @ (ServerMsg::StoryUpdate { advance: Some(_), .. } | ServerMsg::CharacterProgress { .. }
                    | ServerMsg::SceneEnded { .. })) => panic!("incomplete transcript published a result: {message:?}"),
                Ok(_) => {}
            }
        }
        eprintln!("incomplete flower case {index}: rejected or cancelled without saving");
    }
    server.shutdown().await;
    let saved: Vec<_> = {
        let store = Store::open(state.path()).unwrap();
        for bad in &malformed {
            assert_eq!(store.get_progress_pair(bad.account, GameId::Ff7).unwrap(), bad.before);
            assert_eq!(progress_bytes(state.path(), bad.account), bad.bytes, "malformed choices or a deliberate cancel leave both files byte-identical");
        }
        cases.iter().map(|case| {
            let pair = store.get_progress_pair(case.account, GameId::Ff7).unwrap();
            let character = pair.character.as_ref().unwrap();
            assert_eq!(character.gil, if case.bought { case.gil.saturating_sub(1) } else { case.gil });
            assert_eq!((character.exp, character.ap, character.limit, character.level), (42, 7, 33, 6));
            assert_eq!(character.inventory, Some(vec![InventoryEntry { item: 0, count: 3 }]));
            pair
        }).collect()
    };
    let saved_bytes: Vec<_> = cases.iter().map(|case| progress_bytes(state.path(), case.account)).collect();
    let server = TestServer::start_over(config(state.path(), opened.adapter.clone(), rules), Box::new(SharedFf7(opened.adapter.clone()))).await;
    for (case, saved) in cases.iter().zip(&saved) {
        let mut client = Client::connect(server.addr).await;
        let arrived = login(&mut client, &case.token, "md8_2").await;
        assert_eq!(arrived.progress, saved.story.blob);
        assert_eq!(arrived.gil, saved.character.as_ref().unwrap().gil);
        assert_eq!(arrived.pos, flower.place_arrival(Pos::new(-3989.0, 21138.0, 0.0), Some(19)), "authored final Cloud position");
        assert!(!client.drain_for(Duration::from_millis(100)).await.iter().any(|m| matches!(m, ServerMsg::SceneStart { .. })), "completed encounter cannot replay");
    }
    server.shutdown().await;
    let store = Store::open(state.path()).unwrap();
    for ((case, saved), bytes) in cases.iter().zip(saved).zip(saved_bytes) {
        assert_eq!(store.get_progress_pair(case.account, GameId::Ff7).unwrap(), saved, "relogin preserves exact records and no second charge");
        assert_eq!(progress_bytes(state.path(), case.account), bytes, "relogin preserves both serialized files");
    }
}
