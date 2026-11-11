//! Owner-first storage order is independent of the frozen battle's player slots.

use super::*;

#[test]
fn a_follower_owned_scene_keeps_leader_first_battle_slots_through_checkpoint_reopen() {
    let mut rig = Rig::new(3);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    rig.owner.drain();
    let (account, _) = rig.store.create_account("Party leader", 2).unwrap();
    rig.store.put_progression(&Progression::new(account, GameId::Fixture, ORIGINAL.to_vec(), 2)).unwrap();
    let mut character = progress::fresh(&rig.rules, 1).to_record(account, GameId::Fixture, 2);
    character.inventory = Some(vec![InventoryEntry::new(0, 2)]);
    rig.store.put_character_progress(&character).unwrap();
    let state = rig.store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut leader = join(&mut rig.world, state.clone(), vec![state], rig.now);
    assert_eq!(rig.world.party_invite(leader.entity, rig.owner.entity, rig.now), crate::world::PartyOutcome::Done);
    let invite = rig.world.parties.invites().next().unwrap().id;
    assert_eq!(rig.world.party_accept(rig.owner.entity, invite, rig.now), crate::world::PartyOutcome::Done);
    rig.world.start_run(rig.run, SceneRun { scene: SCENE.into(), owner: rig.owner.entity,
        watchers: vec![leader.entity], seed: 19, progress: ORIGINAL.to_vec(), gil: 240,
        answers: vec![], steps: 0, started: rig.now, last_activity: rig.now,
        writes_story: true, preview: false, then: None }, &mut vec![]);
    rig.owner.drain();
    leader.drain();
    rig.request_battle();
    rig.snapshot();
    let (job, done) = rig.pending_batch();
    assert_eq!(job.members[0].expected.pair.story.account_id, rig.owner.account, "the save group is owner first");
    let checkpoint = job.members[0].checkpoint.as_ref().unwrap();
    let payload: Payload = serde_json::from_slice(&checkpoint.replay).unwrap();
    let battle_accounts = vec![leader.account, rig.owner.account];
    assert_eq!(payload.plan.as_ref().unwrap().accounts(), battle_accounts, "combat keeps party leader first");
    for member in &job.members {
        let account = member.expected.pair.story.account_id;
        let slot = member.reservation.as_ref().unwrap().slot;
        assert_eq!(battle_accounts[usize::from(slot)], account, "each saved slot binds battle order, not save order");
    }
    assert_eq!(job.members[0].reservation.as_ref().unwrap().slot, 1);
    let saved = rig.apply(&job);
    rig.ack(done, saved);
    let battle = rig.world.players[&rig.owner.entity].battle.expect("follower-owned checkpoint starts combat");
    assert_eq!(rig.world.players[&leader.entity].battle, Some(battle));

    let owner_account = rig.owner.account;
    let group_before = rig.store.get_scene_group(owner_account, GameId::Fixture).unwrap();
    assert_eq!(group_before[0].pair.story.account_id, owner_account);
    assert_eq!(group_before[0].reservation.as_ref().unwrap().slot, 1);
    let Rig { _dir, store, world: old_world, saves: old_saves, rules, formation, owner, now, .. } = rig;
    let root = store.root().to_path_buf();
    drop(old_world);
    drop(old_saves);
    drop(owner);
    drop(leader);
    drop(store);
    let store = Store::open(root).unwrap();
    let group = store.get_scene_group(owner_account, GameId::Fixture).unwrap();
    assert_eq!(group, group_before);
    let (mut fresh, mut saves) = world(rules, formation);
    let mut returned = join(&mut fresh, group[0].clone(), group.clone(), now);
    fresh.advance_scenes(now);
    let run = fresh.players[&returned.entity].scene.expect("restored follower remains the scene owner");
    assert_eq!(fresh.scenes.runs[&run].owner, returned.entity);
    assert_eq!(fresh.scenes.continuations[&run].payload.plan.as_ref().unwrap().accounts(), battle_accounts);
    assert!(fresh.players[&returned.entity].battle.is_some(), "the retained offline leader does not prevent recovery");
    assert!(returned.drain().iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
    assert!(!fresh.scenes.storage_failed);
    assert!(saves.try_recv().is_err(), "recovery needs no replacement prepare");
    assert_eq!(store.get_scene_group(owner_account, GameId::Fixture).unwrap(), group);
}
