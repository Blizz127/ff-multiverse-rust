//! The players' identity colours on the battle models: which model of a frame carries one (the BODIES of the party
//! members whose players' colours this client knows, and nothing else), how the colours reach the scene from what the
//! client has been told, and, with the owner's own data, before and after pictures through the real renderer.

use super::shots::{open_real, Server, ME};
use super::*;
use crate::avatar::identity::pics::{around, hstack, Img};
use crate::avatar::library::tests::character;
use crate::battle::scene::model::{Attachment, BattleActorModel, BattleModelData, BattleModelSource};
use crate::battle::draw_battle_overlay;
use crate::ui::draw::DrawList;
use crate::ui::testkit::{save, Offscreen};
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntitySnapshot, GameId};
use fmv_protocol::{BattleRules, BattleSetup, ServerMsg, SyntheticRules};

const BLUE: [u8; 3] = [70, 130, 245];
const ORANGE: [u8; 3] = [240, 120, 40];

fn stick() -> fmv_ff7::playable::Character {
    character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3)
}

/// A party member with a sword (a model of its own that moves with it), boxes for the enemies.
struct Armed;

impl BattleModelSource for Armed {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
        let stick = stick();
        let owner = BattleModelData::new(format!("pc{}", who.character), stick.model.clone(), vec![stick.idle.clone()]);
        BattleActorModel::Battle(owner.with_attachment(Attachment::new("sword", stick.model, vec![stick.idle])))
    }

    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
        BattleActorModel::Box(BoxSpec::for_enemy(0, who.max_hp))
    }
}

fn member(i: u64, character: u8) -> BattleMember {
    BattleMember { entity: EntityId(1 + i), account: AccountId(1 + i), name: format!("P{i}"), character, level: 5, weapon: None, armor: None, limit: 0 }
}

fn look(id: u64, name: &str, tint: [u8; 3]) -> EntitySnapshot {
    EntitySnapshot { id: EntityId(id), pos: Pos::default(), facing: 0.0, anim: AnimState::Idle, avatar: AvatarAppearance::new(name, tint, Some("ff7/body/lean".to_string())) }
}

/// A client in the synthetic battle of formation 1 (two dummies) between two party members (entities 1, ours, and 2), of
/// whom the area's snapshot tells the colours given (`None`: the snapshot has not mentioned that player).
fn state_in_battle(own: Option<[u8; 3]>, peer: Option<[u8; 3]>) -> ClientState {
    let area = AreaId { game: GameId::Fixture, key: "k".into() };
    let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), area.clone());
    s.battle.set_rules(Some(Arc::new(SyntheticRules::default())));
    let entities: Vec<EntitySnapshot> = [own.map(|t| look(1, "Me", t)), peer.map(|t| look(2, "You", t))].into_iter().flatten().collect();
    assert!(s.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities, npcs: vec![] }).is_none());
    s.apply(
        0.0,
        ServerMsg::EncounterStart {
            battle_id: BattleId(4),
            formation_id: 1,
            seed: 5,
            party: vec![member(0, 0), member(1, 1)],
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    s
}

/// The frame of the scene `scene_for` makes (and keeps in `slot`) for `s`.
fn frame_of(slot: &mut Option<BattleScene>, kit: &mut Option<ModelKit>, s: &ClientState, now: f64) -> BattleFrame {
    let view = s.battle.view(now).unwrap();
    let scene = scene_for(slot, kit, s, &view, now, &[]).expect("a scene");
    let v = scene.view(1100, 680);
    scene.frame(&v, false)
}

#[test]
fn a_party_members_body_carries_its_players_colour_and_its_weapon_and_the_enemies_do_not() {
    let s = state_in_battle(Some(BLUE), Some(ORANGE));
    let (mut slot, mut kit) = (None, Some(ModelKit::new(Box::new(Armed), None)));
    let frame = frame_of(&mut slot, &mut kit, &s, 1.0);
    // Two members with a sword each, then the two dummies (and no effects: nothing has happened).
    assert_eq!(frame.models.models.len(), 2 * 2 + 2);
    assert_eq!(
        frame.identities,
        [Identity::of(BLUE), Identity::NONE, Identity::of(ORANGE), Identity::NONE, Identity::NONE, Identity::NONE],
        "the bodies carry their players' colours, the swords and the enemies none"
    );
    // Our own is the colour the snapshot gave OUR entity, the other member's the one it gave its entity.
    assert_ne!(frame.identities[0], frame.identities[2]);
    // Colour is not opacity: the tints stay what they were (opaque, one per model).
    assert_eq!(frame.tints.len(), frame.models.models.len());
    assert!(frame.tints.iter().all(|t| *t == Tint::NONE));
    // The frames the GPU draws the stage with carry none of their own.
    assert!(frame.models.identities.is_empty() && frame.models.tints.is_empty());
}

#[test]
fn a_colour_the_client_does_not_know_leaves_that_member_as_it_was() {
    let (mut slot, mut kit) = (None, Some(ModelKit::new(Box::new(Armed), None)));
    // The area has not said who the other member is: only ours is drawn in its colour.
    let frame = frame_of(&mut slot, &mut kit, &state_in_battle(Some(BLUE), None), 1.0);
    assert_eq!(frame.identities, [Identity::of(BLUE), Identity::NONE, Identity::NONE, Identity::NONE, Identity::NONE, Identity::NONE]);
    // Nobody known: nothing changes anywhere (exactly the frame there was before there were colours).
    let (mut slot, mut kit) = (None, Some(ModelKit::new(Box::new(Armed), None)));
    let frame = frame_of(&mut slot, &mut kit, &state_in_battle(None, None), 1.0);
    assert!(frame.identities.iter().all(|i| *i == Identity::NONE));
}

#[test]
fn a_colour_that_arrives_after_the_scene_was_made_applies_one_that_changes_follows_and_one_known_stays() {
    let mut s = state_in_battle(Some(BLUE), None);
    let (mut slot, mut kit) = (None, Some(ModelKit::new(Box::new(Armed), None)));
    assert_eq!(frame_of(&mut slot, &mut kit, &s, 1.0).identities[2], Identity::NONE);
    // The server tells us who the other member is: the next frame shows it (the scene is the same one).
    s.apply(1.5, ServerMsg::PlayerJoined { snapshot: look(2, "You", ORANGE) });
    let frame = frame_of(&mut slot, &mut kit, &s, 2.0);
    assert_eq!(slot.as_ref().map(BattleScene::id), Some(BattleId(4)));
    assert_eq!(frame.identities[..4], [Identity::of(BLUE), Identity::NONE, Identity::of(ORANGE), Identity::NONE]);
    s.apply(2.5, ServerMsg::PlayerJoined { snapshot: look(2, "You", [10, 200, 30]) });
    assert_eq!(frame_of(&mut slot, &mut kit, &s, 3.0).identities[2], Identity::of([10, 200, 30]));
    // The other player drops out in the middle of the fight: the area stops listing it, its model keeps the colour.
    s.apply(3.5, ServerMsg::PlayerLeft { entity: EntityId(2) });
    assert_eq!(s.tint_of(EntityId(2)), None);
    assert_eq!(frame_of(&mut slot, &mut kit, &s, 4.0).identities[2], Identity::of([10, 200, 30]));
}

#[test]
fn the_scene_asks_for_the_colour_of_each_members_own_entity_and_never_of_an_enemy() {
    let engine = SyntheticRules::default().build(&BattleSetup {
        formation_id: 1,
        seed: 7,
        party: vec![member(0, 0), member(1, 1), member(2, 2)],
        preemptive: false,
        back_attack: false,
        battle_speed: 0,
        ticks_per_second: 1000,
    });
    let engine = engine.unwrap();
    let start = SceneStart { id: BattleId(1), party: (0..3).map(|i| member(i, i as u8)).collect(), my_actor: Some(ActorId(0)), formation: None };
    let mut scene = BattleScene::new(&start, Some(&engine), &mut ModelKit::new(Box::new(Armed), None)).unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    scene.show_player_colours(|entity| {
        asked.borrow_mut().push(entity.0);
        (entity.0 == 2).then_some(ORANGE)
    });
    assert_eq!(*asked.borrow(), [1, 2, 3], "one question per party member, by its entity, in the party's order");
    let frame = scene.frame(&scene.view(1100, 680), false);
    // Member 1 (entity 2) alone has a colour: body 0 (index 0), its sword, body 1 ...
    assert_eq!(frame.identities.iter().filter(|i| **i != Identity::NONE).count(), 1);
    assert_eq!(frame.identities[2], Identity::of(ORANGE));
    // A colour once known stays: the player the area stops listing (one that dropped out of the fight) keeps it.
    scene.show_player_colours(|_| None);
    assert_eq!(scene.frame(&scene.view(1100, 680), false).identities[2], Identity::of(ORANGE));
    // And clearing them puts every model back as it was.
    scene.clear_player_colours();
    assert!(scene.frame(&scene.view(1100, 680), false).identities.iter().all(|i| *i == Identity::NONE));
}

// ---------------------------------------------------------------------------------------------
// The owner's own game data
// ---------------------------------------------------------------------------------------------

/// `f` without the models of `bodies` (the colours and tints kept in step: they run in step with the models up to the
/// effects, which come last).
fn without_bodies(mut f: BattleFrame, bodies: &[Arc<AvatarAsset>]) -> BattleFrame {
    let dropped: Vec<bool> = f.models.models.iter().map(|m| bodies.iter().any(|b| Arc::ptr_eq(b, &m.asset))).collect();
    let mut i = 0;
    f.models.models.retain(|_| {
        i += 1;
        !dropped[i - 1]
    });
    let mut i = 0;
    f.tints.retain(|_| {
        i += 1;
        !dropped.get(i - 1).copied().unwrap_or(false)
    });
    let mut i = 0;
    f.identities.retain(|_| {
        i += 1;
        !dropped.get(i - 1).copied().unwrap_or(false)
    });
    f
}

/// Only the models of `bodies`.
fn only_bodies(f: BattleFrame, bodies: &[Arc<AvatarAsset>]) -> BattleFrame {
    let others: Vec<Arc<AvatarAsset>> = f.models.models.iter().map(|m| Arc::clone(&m.asset)).filter(|a| !bodies.iter().any(|b| Arc::ptr_eq(a, b))).collect();
    without_bodies(f, &others)
}

/// Blizz (Barret's combat slot, blue) and Wedge (Tifa's, orange) in the debug formation 301 against its two MPs, the
/// way the window makes the picture: the server's `EncounterStart` and the area's snapshot go to the client state,
/// `scene_for` builds the scene, the party's colours come from what the snapshot said about each member's entity.
/// Before and after are ONE scene at ONE instant: the same frame with the colours shown and taken away, so the pair is
/// pixel-aligned. Writes `battle_identity_*` in `$FMV_SHOT_DIR`, and checks that the party's bodies are the only thing
/// that changes (the enemies, the stage and the weapons are bit for bit as they were).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_identity_before_after() {
    let Some(real) = open_real() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let party = vec![
        BattleMember { entity: EntityId(1), account: ME, name: "Blizz".into(), character: 1, level: 6, weapon: None, armor: None, limit: 0 },
        BattleMember { entity: EntityId(2), account: AccountId(2), name: "Wedge".into(), character: 2, level: 6, weapon: None, armor: None, limit: 0 },
    ];
    let setup = BattleSetup { formation_id: 301, seed: 0xB11, party, preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000 };
    let server = Server { id: BattleId(1), state: real.rules.build(&setup).expect("the formation builds") };
    let area = AreaId { game: GameId::Ff7, key: "md1stin".into() };
    let mut state = ClientState::new(EntityId(1), ME, Pos::default(), area.clone());
    state.battle.set_rules(Some(Arc::clone(&real.rules)));
    state.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities: vec![look(1, "Blizz", BLUE), look(2, "Wedge", ORANGE)], npcs: vec![] });
    assert!(state.apply(0.0, server.start(&setup, real.rules.data_fingerprint())).is_none());
    let (mut slot, mut kit) = (None, Some(real.kit()));
    let battle = state.battle.view(1.0).expect("a view of the battle");
    let events = state.battle.take_events();
    let scene = scene_for(&mut slot, &mut kit, &state, &battle, 1.0, &events).expect("a scene");

    let (w, h) = (1920u32, 1080u32);
    let view = scene.view(w, h);
    let mut overlay = DrawList::new();
    draw_battle_overlay(&battle, &scene.head_anchors(&view), w, h, &mut overlay);
    let nothing = DrawList::new();
    let anchors = scene.screen_anchors(&view);
    let bodies: Vec<Arc<AvatarAsset>> = (0..2).map(|i| Arc::clone(&scene.actors[i].rig.asset)).collect();

    // After: the scene as `scene_for` left it.
    let after_frame = scene.frame(&view, false);
    let shown = after_frame.identities.clone();
    assert_eq!(shown.iter().filter(|i| **i == Identity::of(BLUE)).count(), 1, "{shown:?}");
    assert_eq!(shown.iter().filter(|i| **i == Identity::of(ORANGE)).count(), 1, "{shown:?}");
    assert_eq!(shown.iter().filter(|i| **i != Identity::NONE).count(), 2, "two bodies carry a colour, and nothing else does: {shown:?}");
    let after = gpu.render_battle(w, h, &after_frame, &overlay);
    let after_bare = gpu.render_battle(w, h, &after_frame, &nothing);
    // Before: the same scene without the colours.
    scene.clear_player_colours();
    let before_frame = scene.frame(&view, false);
    assert!(before_frame.identities.iter().all(|i| *i == Identity::NONE));
    let before = gpu.render_battle(w, h, &before_frame, &overlay);
    let before_bare = gpu.render_battle(w, h, &before_frame, &nothing);
    save("battle_identity_before_1920x1080", w, h, &before);
    save("battle_identity_after_1920x1080", w, h, &after);

    // What changed: the party's bodies and nothing else (compared pixel for pixel).
    let box_of = |a: &ActorScreen| {
        let (feet, head) = (a.feet.expect("feet"), a.head.expect("head"));
        let tall = feet[1] - head[1];
        (feet[0] - 0.7 * tall, head[1] - 0.1 * tall, feet[0] + 0.7 * tall, feet[1] + 0.1 * tall)
    };
    let inside = |b: (f32, f32, f32, f32), x: u32, y: u32| (x as f32) >= b.0 && (x as f32) <= b.2 && (y as f32) >= b.1 && (y as f32) <= b.3;
    let party_boxes: Vec<_> = anchors.iter().filter(|a| a.side == Side::Party).map(box_of).collect();
    let enemy_boxes: Vec<_> = anchors.iter().filter(|a| a.side == Side::Enemy).map(box_of).collect();
    let (mut changed, mut outside_party, mut in_enemy, mut per_member) = (0usize, 0usize, 0usize, vec![0usize; party_boxes.len()]);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if before[i..i + 4] != after[i..i + 4] {
                changed += 1;
                match party_boxes.iter().position(|b| inside(*b, x, y)) {
                    Some(m) => per_member[m] += 1,
                    None => outside_party += 1,
                }
                in_enemy += enemy_boxes.iter().filter(|b| inside(**b, x, y)).count();
            }
        }
    }
    eprintln!("{changed} pixels differ between before and after; per party member {per_member:?}; outside the party's boxes {outside_party}; inside an enemy's box {in_enemy}");
    assert!(per_member.iter().all(|&n| n > 2500), "each party member's picture changes: {per_member:?}");
    assert_eq!(outside_party, 0, "nothing but the party's bodies changes");
    assert_eq!(in_enemy, 0, "the enemies are bit for bit as they were");

    // The weapons are as they were: where the weapons alone are drawn and no body is, before and after agree.
    let mut stage = scene.frame(&view, false);
    stage.models.models.clear();
    stage.models.ground.clear();
    let stage_px = gpu.render_battle(w, h, &stage, &nothing);
    let mut weapons = without_bodies(scene.frame(&view, false), &bodies);
    weapons.models.ground.clear();
    let weapon_px = gpu.render_battle(w, h, &weapons, &nothing);
    let mut body_only = only_bodies(scene.frame(&view, false), &bodies);
    body_only.models.ground.clear();
    let body_px = gpu.render_battle(w, h, &body_only, &nothing);
    let (mut weapon_pixels, mut differing) = (0usize, 0usize);
    for i in (0..stage_px.len()).step_by(4) {
        let is_weapon = weapon_px[i..i + 3] != stage_px[i..i + 3];
        let is_body = body_px[i..i + 3] != stage_px[i..i + 3];
        if is_weapon && !is_body {
            weapon_pixels += 1;
            differing += usize::from(before_bare[i..i + 3] != after_bare[i..i + 3]);
        }
    }
    eprintln!("{weapon_pixels} pixels show a weapon and no body (including the enemies' weapons): {differing} of them differ between before and after");
    assert!(weapon_pixels > 500, "the weapons are on the screen ({weapon_pixels} px)");
    assert_eq!(differing, 0, "the weapons keep their own colours");

    // The pictures to look at: each member, before | after, at 3x.
    for (m, (name, colour)) in [("barret_blizz", "blue"), ("tifa_wedge", "orange")].into_iter().enumerate() {
        let a = &anchors[m];
        let (feet, head) = (a.feet.unwrap(), a.head.unwrap());
        let tall = feet[1] - head[1];
        let bbox = (feet[0] - 0.55 * tall, head[1], feet[0] + 0.55 * tall, feet[1]);
        let panels: Vec<Img> = [&before_bare, &after_bare].iter().map(|img| around(img, w, h, bbox, 14.0, 3)).collect();
        let sheet = hstack(&panels);
        save(&format!("battle_identity_before_after_{name}_{colour}"), sheet.w, sheet.h, &sheet.px);
    }
}
