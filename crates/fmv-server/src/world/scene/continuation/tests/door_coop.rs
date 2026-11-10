//! A connected guest follows the door and its linked arrival without acquiring the owner's save.

use super::*;

#[test]
fn a_door_guest_waits_for_shared_arrival_then_returns_to_control_without_a_story_write() {
    let mut rig = door_rig(YARD);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    rig.owner.drain();
    let owner_before = rig.stored();
    let (account, _) = rig.store.create_account("Door guest", 2).unwrap();
    rig.store.put_progression(&Progression::new(account, GameId::Fixture, DOOR_STORY.to_vec(), 2)).unwrap();
    let mut character = progress::fresh(&rig.rules, 1).to_record(account, GameId::Fixture, 2);
    character.gil = 79;
    character.inventory = Some(vec![InventoryEntry::new(0, 3)]);
    rig.store.put_character_progress(&character).unwrap();
    let guest_before = rig.store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut guest = join(&mut rig.world, guest_before.clone(), vec![guest_before.clone()], rig.now);
    assert_eq!(rig.world.party_invite(rig.owner.entity, guest.entity, rig.now), crate::world::PartyOutcome::Done);
    let invite = rig.world.parties.invites().next().unwrap().id;
    assert_eq!(rig.world.party_accept(guest.entity, invite, rig.now), crate::world::PartyOutcome::Done);
    rig.world.start_run(rig.run, SceneRun { scene: DOOR.into(), owner: rig.owner.entity,
        watchers: vec![guest.entity], seed: 19, progress: DOOR_STORY.to_vec(), gil: 240,
        answers: vec![], steps: 0, started: rig.now, last_activity: rig.now,
        writes_story: true, preview: false, then: None }, &mut vec![]);
    rig.owner.drain();
    guest.drain();

    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, done) = rig.pending_batch();
    assert_eq!(finish.operation.kind, SceneOperationKind::Finish);
    assert_eq!(finish.members.len(), 1, "an ephemeral door guest is not added to the owner's transaction");
    assert_eq!(finish.members[0].expected.pair.story.account_id, rig.owner.account);
    assert_eq!(rig.store.get_scene_state(account, GameId::Fixture).unwrap(), guest_before);
    let landed = rig.apply(&finish);
    rig.ack(done, landed);
    for entity in [rig.owner.entity, guest.entity] {
        let player = &rig.world.players[&entity];
        assert_eq!(player.scene, Some(rig.run), "travel does not release a linked arrival's participants");
        assert!(player.transit.is_some());
    }
    let guest_offer = guest.drain().into_iter().find(|message| matches!(message, ServerMsg::AreaChange { .. }))
        .expect("the held guest follows the acknowledged door");
    enter_door_destination(&mut rig);
    rig.tick();
    let owner_waiting = rig.owner.drain();
    let guest_waiting = guest.drain();
    assert!(!owner_waiting.iter().chain(&guest_waiting).any(|message| matches!(message,
        ServerMsg::SceneStart { scene, .. } if scene == ARRIVAL)), "the owner cannot start while its guest is loading");
    assert!(rig.saves.try_recv().is_err(), "the linked arrival is selected only after every connected guest arrives");
    assert_eq!(rig.world.scene_count(), 1);
    assert_eq!(rig.world.players[&guest.entity].scene, Some(rig.run));

    let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = guest_offer else { unreachable!() };
    assert_eq!(rig.world.join_area(guest.entity, &area, &fingerprint, &provenance, rig.now), crate::world::TravelReply::Done);
    rig.tick();
    assert!(!rig.world.scenes.storage_failed);
    assert_eq!(rig.world.scene_count(), 1, "guest entry must not start a separate automatic scene on its own moment 48");
    assert_eq!(rig.world.players[&guest.entity].scene, Some(rig.run));
    let (arrival, done) = rig.pending_batch();
    assert_eq!(arrival.operation.kind, SceneOperationKind::ContinueLanding);
    assert_eq!(arrival.members.len(), 1);
    assert_eq!(arrival.members[0].expected.pair.story.account_id, rig.owner.account);
    for messages in [rig.owner.drain(), guest.drain()] {
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneStart { scene, .. } if scene == ARRIVAL)),
            "shared arrival presentation waits for its durable acknowledgement");
    }
    let arrived = rig.apply(&arrival);
    rig.ack(done, arrived);
    for (entity, role, messages) in [
        (rig.owner.entity, SceneRole::Owner, rig.owner.drain()),
        (guest.entity, SceneRole::Watcher, guest.drain()),
    ] {
        let starts = messages.iter().filter_map(|message| match message {
            ServerMsg::SceneStart { run, scene, owner, participants, you, progress, .. } if scene == ARRIVAL =>
                Some((run, owner, participants, you, progress)),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(starts.len(), 1, "each participant receives exactly one shared arrival");
        let (run, owner, participants, you, progress) = starts[0];
        assert_eq!((*run, *owner, *you), (rig.run, rig.owner.entity, role));
        assert_eq!(progress, DOOR_STORY);
        assert_eq!(participants.len(), 2);
        assert!(participants.iter().any(|participant| participant.entity == entity));
        assert!(participants.iter().any(|participant| participant.entity == rig.owner.entity));
    }
    assert_eq!(rig.store.get_scene_state(account, GameId::Fixture).unwrap(), guest_before);

    request_arrival_complete(&mut rig);
    let (complete, done) = rig.pending_batch();
    assert_eq!(complete.operation.kind, SceneOperationKind::Complete);
    assert_eq!(complete.members.len(), 1);
    assert_eq!(complete.members[0].expected.pair.story.account_id, rig.owner.account);
    assert_eq!(complete.members[0].next_story.as_deref(), Some(ARRIVED_STORY));
    let completed = rig.apply(&complete);
    for entity in [rig.owner.entity, guest.entity] {
        let player = &rig.world.players[&entity];
        assert_eq!((player.snapshot.pos, player.scene), (DoorHost::landing().arrival, Some(rig.run)),
            "disk success alone cannot move or release either participant");
    }
    for messages in [rig.owner.drain(), guest.drain()] {
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. })));
    }
    rig.ack(done, completed);
    let control = DoorHost { final_field: YARD }.control();
    for (entity, messages) in [(rig.owner.entity, rig.owner.drain()), (guest.entity, guest.drain())] {
        let placed = messages.iter().position(|message| matches!(message,
            ServerMsg::AreaSnapshot { area, entities, .. } if area.key == YARD && entities.iter().any(|snapshot|
                snapshot.id == entity && snapshot.pos == control.arrival && Some(snapshot.facing) == control.arrival_facing)))
            .expect("each client receives its final authoritative pose");
        let released = messages.iter().position(|message| matches!(message,
            ServerMsg::SceneEnded { run, outcome: SceneOutcome::Finished } if *run == rig.run)).expect("shared arrival releases both participants");
        assert!(placed < released, "the final pose precedes control returning");
        assert_eq!(messages.iter().filter(|message| matches!(message, ServerMsg::SceneEnded { .. })).count(), 1);
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::AreaChange { .. } | ServerMsg::SceneStart { .. })));
        let player = &rig.world.players[&entity];
        assert_eq!((player.snapshot.pos, player.snapshot.facing, player.scene), (control.arrival, control.arrival_facing.unwrap(), None));
        assert!(player.transit.is_none() && player.battle.is_none());
    }
    assert_eq!(rig.world.players[&rig.owner.entity].progression, ARRIVED_STORY);
    assert_eq!(rig.world.players[&guest.entity].progression, DOOR_STORY);
    assert_eq!(door_resources(&rig.stored()), door_resources(&owner_before));
    assert_eq!(rig.store.get_scene_state(account, GameId::Fixture).unwrap(), guest_before,
        "guest story, wallet, inventory and durable metadata remain byte-for-byte unchanged");
    assert_eq!(rig.world.players[&guest.entity].progress, Some(door_resources(&guest_before).0));
    assert_eq!(rig.world.players[&guest.entity].inventory.as_ref().unwrap().entries(), character.inventory.as_deref().unwrap());
    rig.tick();
    assert_eq!(rig.world.scene_count(), 0);
    assert!(!rig.world.scenes.storage_failed);
    assert!(rig.saves.try_recv().is_err(), "the guest queues no independent story completion");
    assert!(!guest.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { .. })));
}

#[test]
fn an_owner_rejoining_after_the_arrival_commit_refreshes_the_retained_guests_participants() {
    let mut rig = door_rig(YARD);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    rig.owner.drain();
    let owner_before = rig.stored();
    let (account, _) = rig.store.create_account("Rejoining owner's guest", 2).unwrap();
    rig.store.put_progression(&Progression::new(account, GameId::Fixture, DOOR_STORY.to_vec(), 2)).unwrap();
    let mut character = progress::fresh(&rig.rules, 1).to_record(account, GameId::Fixture, 2);
    character.gil = 79;
    character.inventory = Some(vec![InventoryEntry::new(0, 3)]);
    rig.store.put_character_progress(&character).unwrap();
    let guest_before = rig.store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut guest = join(&mut rig.world, guest_before.clone(), vec![guest_before.clone()], rig.now);
    assert_eq!(rig.world.party_invite(rig.owner.entity, guest.entity, rig.now), crate::world::PartyOutcome::Done);
    let invite = rig.world.parties.invites().next().unwrap().id;
    assert_eq!(rig.world.party_accept(guest.entity, invite, rig.now), crate::world::PartyOutcome::Done);
    rig.world.start_run(rig.run, SceneRun { scene: DOOR.into(), owner: rig.owner.entity,
        watchers: vec![guest.entity], seed: 19, progress: DOOR_STORY.to_vec(), gil: 240,
        answers: vec![], steps: 0, started: rig.now, last_activity: rig.now,
        writes_story: true, preview: false, then: None }, &mut vec![]);
    rig.owner.drain();
    guest.drain();

    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, done) = rig.pending_batch();
    assert_eq!(finish.operation.kind, SceneOperationKind::Finish);
    let landed = rig.apply(&finish);
    rig.ack(done, landed);
    let guest_offer = guest.drain().into_iter().find(|message| matches!(message, ServerMsg::AreaChange { .. })).unwrap();
    enter_door_destination(&mut rig);
    assert!(rig.saves.try_recv().is_err());
    let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = guest_offer else { unreachable!() };
    assert_eq!(rig.world.join_area(guest.entity, &area, &fingerprint, &provenance, rig.now), crate::world::TravelReply::Done);
    rig.tick();
    let (arrival, done) = rig.pending_batch();
    assert_eq!(arrival.operation.kind, SceneOperationKind::ContinueLanding);
    assert_eq!(arrival.members.len(), 1);
    let old_owner = rig.owner.entity;
    assert!(rig.world.leave(old_owner), "the owner disappears after the arrival captured its participant ids");
    assert_eq!(rig.world.players[&guest.entity].scene, Some(rig.run));
    assert!(matches!(rig.stored().checkpoint.as_ref().unwrap().phase, ScenePhase::Landing { .. }));
    let arrived = rig.apply(&arrival);
    rig.ack(done, arrived);
    let resumed = rig.stored();
    assert!(matches!(resumed.checkpoint.as_ref().unwrap().phase, ScenePhase::Resume { segment: 0 }));
    assert_eq!(resumed.checkpoint.as_ref().unwrap().scene_key, ARRIVAL);
    assert_eq!(rig.world.players[&guest.entity].scene, Some(rig.run));
    guest.drain();

    let group = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    assert_eq!(group, vec![resumed.clone()], "only the owner's arrival is durable");
    let returned = join(&mut rig.world, resumed.clone(), group, rig.now);
    let new_owner = returned.entity;
    assert_ne!(new_owner, old_owner);
    rig.owner = returned;
    assert!(rig.world.players[&new_owner].transit.is_some(), "the reconnect must load the destination before replay is shared");
    assert!(!guest.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { .. })),
        "participant refresh cannot precede the new owner's destination handshake");
    enter_door_destination(&mut rig);
    rig.tick();
    for (role, messages) in [(SceneRole::Owner, rig.owner.drain()), (SceneRole::Watcher, guest.drain())] {
        let starts = messages.iter().filter_map(|message| match message {
            ServerMsg::SceneStart { run, scene, owner, participants, you, progress, replay: Some(replay), .. }
                if scene == ARRIVAL => Some((run, owner, participants, you, progress, replay)),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(starts.len(), 1, "both the owner and retained guest receive the corrected shared replay");
        let (run, owner, participants, you, progress, replay) = starts[0];
        assert_eq!((*run, *owner, *you), (rig.run, new_owner, role));
        assert_eq!(progress, DOOR_STORY);
        assert_eq!(replay.source, ARRIVAL_SOURCE);
        assert!(replay.events.is_empty());
        assert_eq!(participants.len(), 2);
        assert!(participants.iter().any(|participant| participant.entity == new_owner));
        assert!(participants.iter().any(|participant| participant.entity == guest.entity));
        assert!(!participants.iter().any(|participant| participant.entity == old_owner),
            "a hidden participant must identify the newly connected owner, not the departed entity");
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. })));
    }
    for entity in [new_owner, guest.entity] {
        let player = &rig.world.players[&entity];
        assert_eq!((player.scene, player.area.as_str()), (Some(rig.run), YARD));
        assert!(player.transit.is_none() && player.battle.is_none());
        assert_eq!(player.progression, DOOR_STORY);
    }
    assert_eq!(rig.stored(), resumed, "rejoining only reconstructs the acknowledged checkpoint");
    assert_eq!(door_resources(&rig.stored()), door_resources(&owner_before));
    assert_eq!(rig.store.get_scene_state(account, GameId::Fixture).unwrap(), guest_before);
    assert_eq!(rig.world.players[&guest.entity].progress, Some(door_resources(&guest_before).0));
    assert_eq!(rig.world.scene_count(), 1);
    assert!(!rig.world.scenes.storage_failed);
    assert!(rig.saves.try_recv().is_err(), "participant refresh cannot rewrite either account");
}
