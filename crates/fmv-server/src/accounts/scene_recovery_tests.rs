//! Account admission must carry one coherent resource snapshot and its complete recovery group.

use super::*;
use fmv_core::InventoryEntry;
use fmv_persist::{Progression, SceneCheckpoint, SceneOperation, SceneOperationKind, ScenePhase, SceneReservation, SceneState};
use fmv_protocol::SyntheticRules;
use std::fs;

const GAME: GameId = GameId::Ff7;
const NOW: u64 = 1_700_000_000;
const INSTANCE: [u8; 32] = [17; 32];
const OWNER: usize = 1;

struct Member {
    account: AccountId,
    token: Secret,
}

fn registration(name: &str, slot: u8) -> Registration {
    validate_registration(GAME, name, &NewCharacter { tint: [1, 2, 3], body: None, slot: Some(slot) }).unwrap()
}

fn fixture() -> (tempfile::TempDir, Store, Vec<Member>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state")).unwrap();
    let members = ["First witness", "Scene owner", "Second witness"].into_iter().enumerate().map(|(index, name)| {
        let registered = register(&store, GAME, registration(name, index as u8 + 1), NOW, || {}).unwrap();
        store.mark_claimed(registered.account).unwrap();
        store.put_progression(&Progression::new(registered.account, GAME, vec![1, index as u8], NOW)).unwrap();
        let progress = CharacterProgress {
            exp: 100 + index as u32,
            gil: 240 + index as u32,
            inventory: Some(vec![InventoryEntry::new(index as u16, 2)]),
            ..CharacterProgress::new(registered.account, GAME, index as u8 + 1, 6, NOW)
        };
        store.put_character_progress(&progress).unwrap();
        Member { account: registered.account, token: Secret::new(registered.issued_token.unwrap()) }
    }).collect();
    (dir, store, members)
}

fn operation(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal: 0, attempt: 0, kind }
}

fn prepare(store: &Store, members: &[Member]) -> (Vec<SceneState>, Vec<SceneState>) {
    let accounts: Vec<_> = members.iter().map(|member| member.account).collect();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let mut next = before.clone();
    next[OWNER].checkpoint = Some(SceneCheckpoint {
        instance: INSTANCE,
        scene_key: "fixture/reconnect-scene".into(),
        source_hash: [23; 32],
        replay_version: 1,
        original_story: before[OWNER].pair.story.blob.clone(),
        original_gil: before[OWNER].pair.character.as_ref().unwrap().gil,
        seed: 42,
        replay: vec![4, 5],
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 12, battle_seed: 100 },
        consumed_battles: 0,
    });
    // Neither account order nor battle slot identifies the owner: it has slot 2.
    for (state, slot) in next.iter_mut().zip([0, 2, 1]) {
        state.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot });
    }
    let prepared = store.commit_scene_batch(&operation(SceneOperationKind::PrepareBattle), b"prepare", &before, &next).unwrap().current;
    (before, prepared)
}

fn login_member(store: &Store, member: &Member, with_rules: bool) -> Result<Login, LoginError> {
    if with_rules {
        login_with_token_and_rules(store, GAME, &member.token, [9; 3], NOW + 5, &SyntheticRules::default())
    } else {
        login_with_token(store, GAME, &member.token, [9; 3], NOW + 5)
    }
}

fn assert_own_snapshot(login: &Login, state: &SceneState) {
    assert_eq!(login.account, state.pair.story.account_id);
    assert_eq!(login.scene_state.as_ref(), Some(state));
    assert_eq!(login.progression, state.pair.story.blob);
    assert_eq!(login.progress.stored, state.pair.character.as_ref().map(Progress::from_record));
    assert_eq!(login.progress.inventory, state.pair.character.as_ref().and_then(|record| record.inventory.clone()));
    assert!(login.issued_token.is_none());
}

#[test]
fn every_reserved_login_recovers_its_own_resources_and_the_complete_owner_first_group() {
    let (_dir, store, members) = fixture();
    let (_, prepared) = prepare(&store, &members);
    let expected_group = vec![prepared[OWNER].clone(), prepared[0].clone(), prepared[2].clone()];
    let root = store.root().to_path_buf();
    drop(store);
    let store = Store::open(root).unwrap();
    for with_rules in [false, true] {
        for (index, member) in members.iter().enumerate() {
            let login = login_member(&store, member, with_rules).unwrap();
            assert_own_snapshot(&login, &prepared[index]);
            assert_eq!(login.scene_group, expected_group, "owner and witnesses recover the same complete group");
            assert_eq!(login.scene_group[0].reservation.as_ref().unwrap().slot, 2);
        }
    }
}

#[test]
fn login_keeps_the_settled_group_until_final_landing_releases_its_reservations() {
    let (_dir, store, members) = fixture();
    let (before, prepared) = prepare(&store, &members);
    let old_login = login_member(&store, &members[OWNER], false).unwrap();
    let mut next = prepared.clone();
    for (index, state) in next.iter_mut().enumerate() {
        let progress = state.pair.character.as_mut().unwrap();
        progress.gil += 10 + index as u32;
        progress.exp += 50;
        progress.inventory.as_mut().unwrap().push(InventoryEntry::new(9, 1));
        progress.updated_at = NOW + 1;
    }
    next[OWNER].pair.story.blob.push(8);
    next[OWNER].pair.story.updated_at = NOW + 1;
    let checkpoint = next[OWNER].checkpoint.as_mut().unwrap();
    checkpoint.phase = ScenePhase::Resume { segment: 1 };
    checkpoint.consumed_battles = 1;
    checkpoint.replay.push(6);
    let settled = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle), b"won", &prepared, &next).unwrap().current;
    // A retransmitted preparation cannot resurrect the old checkpoint or resources.
    let duplicate = store.commit_scene_batch(&operation(SceneOperationKind::PrepareBattle), b"prepare", &before, &prepared).unwrap();
    assert_eq!(duplicate.current, settled);
    let root = store.root().to_path_buf();
    drop(store);
    let store = Store::open(&root).unwrap();
    let expected_group = vec![settled[OWNER].clone(), settled[0].clone(), settled[2].clone()];
    for with_rules in [false, true] {
        for (index, member) in members.iter().enumerate() {
            let login = login_member(&store, member, with_rules).unwrap();
            assert_own_snapshot(&login, &settled[index]);
            assert_eq!(login.scene_group, expected_group, "battle settlement retains the full scene cohort");
            assert_eq!(login.scene_state.as_ref().unwrap().reservation, prepared[index].reservation);
            assert_eq!(login.scene_state.as_ref().unwrap().checkpoint.is_some(), index == OWNER);
        }
    }
    assert_eq!(settled[OWNER].checkpoint.as_ref().unwrap().phase, ScenePhase::Resume { segment: 1 });
    let mut final_state = settled.clone();
    for state in &mut final_state { state.reservation = None; }
    final_state[OWNER].pair.story.blob.push(9);
    final_state[OWNER].pair.story.updated_at = NOW + 2;
    let checkpoint = final_state[OWNER].checkpoint.as_mut().unwrap();
    checkpoint.phase = ScenePhase::Landing { destination: vec![8, 12] };
    checkpoint.replay.push(7);
    let finish = SceneOperation { ordinal: 1, ..operation(SceneOperationKind::Finish) };
    let landed = store.commit_scene_batch(&finish, b"landed", &settled, &final_state).unwrap().current;
    let duplicate = store.commit_scene_batch(&operation(SceneOperationKind::PrepareBattle), b"prepare", &before, &prepared).unwrap();
    assert_eq!(duplicate.current, landed, "an old preparation cannot resurrect a released reservation");
    drop(store);
    let store = Store::open(root).unwrap();
    for with_rules in [false, true] {
        for (index, member) in members.iter().enumerate() {
            let login = login_member(&store, member, with_rules).unwrap();
            assert_own_snapshot(&login, &landed[index]);
            assert_eq!(login.scene_group, vec![landed[index].clone()], "landing releases each guest to its own latest state");
            assert!(login.scene_state.as_ref().unwrap().reservation.is_none());
            assert_eq!(login.scene_state.as_ref().unwrap().checkpoint.is_some(), index == OWNER);
        }
    }
    assert_eq!(old_login.scene_state.as_ref(), Some(&prepared[OWNER]), "an earlier admission stays an old snapshot, not an alias");
}

#[test]
fn missing_corrupt_or_inconsistent_members_refuse_both_owner_and_witness_logins() {
    for damage in ["missing", "corrupt", "other boundary", "missing roster"] {
        let (_dir, store, members) = fixture();
        prepare(&store, &members);
        let path = store.root().join("progression").join(members[2].account.0.to_string()).join("Ff7.character.json");
        match damage {
            "missing" => fs::remove_file(&path).unwrap(),
            "corrupt" => fs::write(&path, b"{not a character record").unwrap(),
            _ => {
                let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if damage == "other boundary" { record["reservation"]["ordinal"] = 1.into(); }
                else { record.as_object_mut().unwrap().remove("scene_roster"); }
                fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
            }
        }
        let damaged = fs::read(&path).ok();
        for with_rules in [false, true] {
            for index in [OWNER, 0] {
                let error = login_member(&store, &members[index], with_rules).unwrap_err();
                assert!(matches!(error, LoginError::Internal(_)), "{damage}: {error:?}");
                assert_eq!(error.code(), RejectCode::Internal);
                assert_eq!(error.client_text(), "the account store failed; try again later");
                assert_eq!(fs::read(&path).ok(), damaged, "login cannot fabricate or repair the missing group member");
            }
        }
    }
}

#[test]
fn registration_returns_no_recovery_state_or_group_with_or_without_battle_rules() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let plain = register(&store, GAME, registration("Plain", 1), NOW, || {}).unwrap();
    let seeded = register_with_rules(&store, GAME, registration("Seeded", 2), NOW, || {}, &SyntheticRules::default()).unwrap();
    for registered in [plain, seeded] {
        assert!(registered.scene_state.is_none());
        assert!(registered.scene_group.is_empty());
        assert!(registered.issued_token.is_some());
        assert!(registered.progression.is_empty());
    }
}
