//! The players' identity colours on the battle models: which model of a frame carries one (the BODIES of the party
//! members whose players' colours this client knows, and nothing else), how the colours reach the scene from what the
//! client has been told, and, with the owner's own data, before and after pictures through the real renderer.

use super::shots::{drawn_at, open_real, Real, Server, ME};
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
    BattleMember::player(EntityId(1 + i), AccountId(1 + i), format!("P{i}"), character, 5, 0)
}

fn look(id: u64, name: &str, tint: [u8; 3]) -> EntitySnapshot {
    look_of(id, name, tint, "ff7/body/lean")
}

/// What the area's snapshot says about a player's entity: its name and colour, and the binding (`ff7/body/<body>`, or the
/// canon look `ff7/cloud` and so on that an account of before the creator may have kept) the game draws it as on the field.
fn look_of(id: u64, name: &str, tint: [u8; 3], binding: &str) -> EntitySnapshot {
    EntitySnapshot { id: EntityId(id), pos: Pos::default(), facing: 0.0, anim: AnimState::Idle, avatar: AvatarAppearance::new(name, tint, Some(binding.to_string())) }
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
    let start = SceneStart { id: BattleId(1), party: (0..3).map(|i| member(i, i as u8)).collect(), bindings: Vec::new(), my_actor: Some(ActorId(0)), formation: None };
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
// Canon companions first, the players after them (protocol v9)
// ---------------------------------------------------------------------------------------------

/// The roster of a battle on the first reactor mission with two players: the canon Cloud and Barret (server-controlled,
/// no entity), then the two players of entities 1 (ours) and 2, in party order.
fn canon_then_two_players() -> Vec<BattleMember> {
    vec![BattleMember::companion("Cloud", 0, 5, 0), BattleMember::companion("Barret", 1, 5, 0), member(0, 0), member(1, 1)]
}

/// A client in the synthetic battle of formation 1 whose party is [`canon_then_two_players`], the area's snapshot giving
/// the colours of the two PLAYERS' entities and (to catch a lookup that goes by position) of two entities that stand for
/// nobody in the battle, which are the ones a companion's index would find if the scene took a player's entity by
/// the position of the member in the roster.
fn state_with_companions(own: [u8; 3], peer: [u8; 3]) -> ClientState {
    let area = AreaId { game: GameId::Fixture, key: "k".into() };
    let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), area.clone());
    s.battle.set_rules(Some(Arc::new(SyntheticRules::default())));
    let bystander = [10, 20, 30];
    let entities = vec![look(1, "Me", own), look(2, "You", peer), look(3, "Bystander", bystander), look(4, "Another", bystander)];
    assert!(s.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities, npcs: vec![] }).is_none());
    s.apply(
        0.0,
        ServerMsg::EncounterStart {
            battle_id: BattleId(6),
            formation_id: 1,
            seed: 5,
            party: canon_then_two_players(),
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    s
}

/// With the canon party first in the roster, the players' colours land on the PLAYERS' bodies (actors 2 and 3), and
/// Cloud's and Barret's (actors 0 and 1) carry none: the companions have no entity to look a colour up by, and a
/// player's is found by its own entity, never by where it stands in the roster.
#[test]
fn the_players_colours_land_on_the_players_and_never_on_the_canon_companions() {
    let s = state_with_companions(BLUE, ORANGE);
    let start = s.battle.scene_start().expect("in the battle");
    assert_eq!(start.my_actor, Some(ActorId(2)), "we are the third member of the roster, after Cloud and Barret");
    assert_eq!(start.party.iter().map(BattleMember::entity).collect::<Vec<_>>(), [None, None, Some(EntityId(1)), Some(EntityId(2))]);
    let (mut slot, mut kit) = (None, Some(ModelKit::new(Box::new(Armed), None)));
    let frame = frame_of(&mut slot, &mut kit, &s, 1.0);
    // Four members with a sword each, then the two dummies.
    assert_eq!(frame.models.models.len(), 4 * 2 + 2);
    assert_eq!(
        frame.identities,
        [
            Identity::NONE,
            Identity::NONE,
            Identity::NONE,
            Identity::NONE,
            Identity::of(BLUE),
            Identity::NONE,
            Identity::of(ORANGE),
            Identity::NONE,
            Identity::NONE,
            Identity::NONE
        ],
        "Cloud and Barret (and their swords) carry no colour; our body carries ours, the other player's its own"
    );
    // Entities 3 and 4 are in the area with a colour of their own: nobody in the battle is them, so nobody wears it.
    assert!(frame.identities.iter().all(|i| *i != Identity::of([10, 20, 30])));
}

/// The scene asks for a colour once for each PLAYER, by its entity and in roster order, and never for a companion.
#[test]
fn the_scene_asks_for_the_colour_of_each_players_entity_and_never_of_a_companion() {
    let party = canon_then_two_players();
    let engine = SyntheticRules::default()
        .build(&BattleSetup { formation_id: 1, seed: 7, party: party.clone(), preemptive: false, back_attack: false, battle_speed: 0, ticks_per_second: 1000 })
        .unwrap();
    let start = SceneStart { id: BattleId(1), party, bindings: Vec::new(), my_actor: Some(ActorId(2)), formation: None };
    let mut scene = BattleScene::new(&start, Some(&engine), &mut ModelKit::new(Box::new(Armed), None)).unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    scene.show_player_colours(|entity| {
        asked.borrow_mut().push(entity.0);
        Some(if entity.0 == 1 { BLUE } else { ORANGE })
    });
    assert_eq!(*asked.borrow(), [1, 2], "one question per player, by its entity, past the companions");
    let frame = scene.frame(&scene.view(1100, 680), false);
    assert_eq!(frame.identities.iter().filter(|i| **i != Identity::NONE).count(), 2, "only the two players' bodies");
    assert_eq!((frame.identities[4], frame.identities[6]), (Identity::of(BLUE), Identity::of(ORANGE)));
    assert!(frame.identities[..4].iter().all(|i| *i == Identity::NONE), "Cloud and Barret keep their own colours");
}

// ---------------------------------------------------------------------------------------------
// The owner's own game data
// ---------------------------------------------------------------------------------------------

/// The assumption the face rule rests on, measured on the battle models too: the `overlay` flag (textured opaque parts:
/// the face decals) covers well under 1% of the model of every combat slot (the battle models, like the field ones, are
/// vertex-coloured), so "face" is the decals in battle as in the field and does not take the bodies out of the look.
/// Aggregates only.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY)"]
fn real_identity_battle_models_are_vertex_coloured_with_face_decals_only() {
    use crate::avatar::identity::measure::shares;
    let Some(real) = open_real() else { return };
    let mut kit = real.kit();
    for character in [0u8, 1, 2, 3, 4, 5, 6, 7, 8] {
        let model = kit.source.party_member(&PartyLook::companion(ActorId(0), character, true));
        let rig = kit.rigs.resolve(&model, BoxSpec::for_party(0));
        let s = shares(&rig.asset.mesh);
        eprintln!(
            "battle slot {character}: {} triangles: overlay {:.1}% textured {:.1}% skin-coloured {:.1}% pure black (under 8 of 255) {:.1}%",
            s.triangles,
            s.overlay * 100.0,
            s.textured * 100.0,
            s.skin * 100.0,
            s.black * 100.0
        );
        assert!(!rig.attachments.is_empty(), "slot {character} carries its weapon, which is never given the colour");
        assert!(s.overlay < 0.02 && (s.overlay - s.textured).abs() < 1e-6, "slot {character}: the overlay flag is the face decals");
    }
}

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

/// The canon Cloud and Barret (server-controlled companions: their own battle models, no colour) and two players, Blizz
/// (Barret's stat curve, the lean body, blue) and Wedge (Tifa's curve, the slight body, orange) in the debug formation 301
/// against its two MPs, the way the window makes the picture: the server's `EncounterStart` and the area's snapshot go to
/// the client state, `scene_for` builds the scene, the players' colours come from what the snapshot said about each
/// PLAYER's entity. Before and after are ONE scene at ONE instant: the same frame with the colours shown and taken
/// away, so the pair is pixel-aligned. Writes `battle_identity_*` in `$FMV_SHOT_DIR`, and checks that the players'
/// bodies are the only thing that changes (the companions, the enemies, the stage and the weapons are bit for bit as
/// they were).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_identity_before_after() {
    let Some(real) = open_real() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let party = vec![
        BattleMember::companion("Cloud", 0, 6, 0),
        BattleMember::companion("Barret", 1, 6, 0),
        BattleMember::player(EntityId(1), ME, "Blizz", 1, 6, 0),
        BattleMember::player(EntityId(2), AccountId(2), "Wedge", 2, 6, 0),
    ];
    let setup = BattleSetup { formation_id: 301, seed: 0xB11, party, preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000 };
    let server = Server { id: BattleId(1), state: real.rules.build(&setup).expect("the formation builds") };
    let area = AreaId { game: GameId::Ff7, key: "md1stin".into() };
    let mut state = ClientState::new(EntityId(1), ME, Pos::default(), area.clone());
    state.battle.set_rules(Some(Arc::clone(&real.rules)));
    let looks = vec![look(1, "Blizz", BLUE), look_of(2, "Wedge", ORANGE, "ff7/body/slight")];
    state.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities: looks, npcs: vec![] });
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
    // The players' bodies (actors 2 and 3, after the two companions).
    let bodies: Vec<Arc<AvatarAsset>> = (2..4).map(|i| Arc::clone(&scene.actors[i].rig.asset)).collect();

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
    assert_eq!(party_boxes.len(), 4, "Cloud, Barret and the two players");
    let (companion_boxes, player_boxes) = party_boxes.split_at(2);
    let (mut changed, mut outside_party, mut in_enemy, mut only_in_companions) = (0usize, 0usize, 0usize, 0usize);
    let mut per_player = vec![0usize; player_boxes.len()];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if before[i..i + 4] != after[i..i + 4] {
                changed += 1;
                let in_players: Vec<usize> = (0..player_boxes.len()).filter(|&m| inside(player_boxes[m], x, y)).collect();
                for &m in &in_players {
                    per_player[m] += 1;
                }
                if in_players.is_empty() {
                    if companion_boxes.iter().any(|b| inside(*b, x, y)) {
                        only_in_companions += 1;
                    } else {
                        outside_party += 1;
                    }
                }
                in_enemy += enemy_boxes.iter().filter(|b| inside(**b, x, y)).count();
            }
        }
    }
    eprintln!(
        "{changed} pixels differ between before and after; per player {per_player:?}; only inside a companion's box {only_in_companions}; outside the party's boxes {outside_party}; inside an enemy's box {in_enemy}"
    );
    assert!(per_player.iter().all(|&n| n > 1500), "each player's picture changes: {per_player:?}");
    assert_eq!(only_in_companions, 0, "Cloud and Barret are bit for bit as they were: the colours are the players'");
    assert_eq!(outside_party, 0, "nothing but the players' bodies changes");
    assert_eq!(in_enemy, 0, "the enemies are bit for bit as they were");

    // Everything but the players' bodies (the companions' bodies and weapons, the enemies' and theirs) is as it was:
    // where those alone are drawn and no player's body is, before and after agree.
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
    eprintln!("{weapon_pixels} pixels show a companion, an enemy or a weapon and no player's body: {differing} of them differ between before and after");
    assert!(weapon_pixels > 500, "the companions, the enemies and the weapons are on the screen ({weapon_pixels} px)");
    assert_eq!(differing, 0, "the companions, the enemies and the weapons keep their own colours");

    // The pictures to look at: each player, before | after, at 3x.
    for (m, (name, colour)) in [("blizz_lean", "blue"), ("wedge_slight", "orange")].into_iter().enumerate() {
        let a = &anchors[2 + m];
        let (feet, head) = (a.feet.unwrap(), a.head.unwrap());
        let tall = feet[1] - head[1];
        let bbox = (feet[0] - 0.55 * tall, head[1], feet[0] + 0.55 * tall, feet[1]);
        let panels: Vec<Img> = [&before_bare, &after_bare].iter().map(|img| around(img, w, h, bbox, 14.0, 3)).collect();
        let sheet = hstack(&panels);
        save(&format!("battle_identity_before_after_{name}_{colour}"), sheet.w, sheet.h, &sheet.px);
    }
}

// ---------------------------------------------------------------------------------------------
// The owner's cases, as pictures (protocol v9)
// ---------------------------------------------------------------------------------------------

/// A battle of the debug formation 301 (the two MPs) the way the window makes it, over the owner's own data: the server's
/// `EncounterStart` and the area's snapshot (`looks`: how the game draws each PLAYER's entity, in its colour) go to a client
/// state, and `scene_for` builds the scene from them with `kit`. ONE kit serves every battle of a test, as the window's
/// does: a kit of its own for each battle would make every model a new asset for the GPU, which keeps 32 at most and
/// starts over (mid-frame) beyond that.
struct RealBattle {
    scene: BattleScene,
    battle: BattleView,
}

fn real_battle(real: &Real, kit: &mut Option<ModelKit>, party: Vec<BattleMember>, looks: Vec<EntitySnapshot>, seed: u64) -> RealBattle {
    let setup = BattleSetup { formation_id: 301, seed, party, preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000 };
    let server = Server { id: BattleId(1), state: real.rules.build(&setup).expect("the formation builds") };
    let area = AreaId { game: GameId::Ff7, key: "md1stin".into() };
    let mut state = ClientState::new(EntityId(1), ME, Pos::default(), area.clone());
    state.battle.set_rules(Some(Arc::clone(&real.rules)));
    state.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities: looks, npcs: vec![] });
    assert!(state.apply(0.0, server.start(&setup, real.rules.data_fingerprint())).is_none());
    let mut slot = None;
    let battle = state.battle.view(1.0).expect("a view of the battle");
    let events = state.battle.take_events();
    scene_for(&mut slot, kit, &state, &battle, 1.0, &events).expect("a scene");
    RealBattle { scene: slot.expect("a scene"), battle }
}

/// One of the owner's cases: the players of a battle in party order, each with its name, its stat curve (internal: who it
/// fights like), the binding the field draws it as (a placeholder body, or a canon look an old account kept) and its colour.
struct Case {
    name: &'static str,
    players: &'static [(&'static str, u8, &'static str, [u8; 3])],
}

const GREEN: [u8; 3] = [60, 200, 90];

const CASES: [Case; 5] = [
    // The DoD picture: the canon pair and two players.
    Case { name: "two_players", players: &[("Blizzy", 1, "ff7/body/lean", BLUE), ("Wedge", 2, "ff7/body/slight", ORANGE)] },
    // Up to five in the party: the canon pair and the three players the party allows.
    Case { name: "three_players", players: &[("Blizzy", 1, "ff7/body/lean", BLUE), ("Wedge", 2, "ff7/body/slight", ORANGE), ("Biggs", 4, "ff7/body/sturdy", GREEN)] },
    // The owner's bug: a new character on Barret's curve (his "Blizzy"), alone, was drawn as Barret and fought alone.
    Case { name: "solo_new_character_on_barrets_curve", players: &[("Blizzy", 1, "ff7/body/lean", BLUE)] },
    // An account of before the creator that never chose a look (a headless bot's, say), on Cloud's curve, alone: it was
    // given the default placeholder body when it was migrated, so Cloud is the canon one only.
    Case { name: "solo_migrated_account_with_no_look_on_clouds_curve", players: &[("Blizz", 0, "ff7/body/lean", ORANGE)] },
    // An account of before the creator that CHOSE Cloud's own look (the canon `ff7/cloud`, which the migration keeps as its
    // binding: what every player sees of it on the field): its own body in battle is the field's Cloud model at the
    // field's scale, tinted, beside the canon Cloud's battle model (`fmv-characters restyle` gives such an account a
    // placeholder body of its own).
    Case { name: "solo_migrated_account_with_clouds_field_look", players: &[("Blizz", 0, "ff7/cloud", ORANGE)] },
];

/// The canon characters of the first reactor mission, in the order the roster has them.
const CANON: [(&str, u8); 2] = [("Cloud", 0), ("Barret", 1)];

/// What `case`'s roster is: Cloud and Barret as companions, then the players (the first is ours, entity 1).
fn roster_of_case(case: &Case) -> (Vec<BattleMember>, Vec<EntitySnapshot>) {
    let mut party: Vec<BattleMember> = CANON.iter().map(|(name, character)| BattleMember::companion(*name, *character, 6, 0)).collect();
    let mut looks = Vec::new();
    for (i, (name, curve, body, colour)) in case.players.iter().enumerate() {
        let account = if i == 0 { ME } else { AccountId(1 + i as u64) };
        party.push(BattleMember::player(EntityId(1 + i as u64), account, *name, *curve, 6, 0));
        looks.push(look_of(1 + i as u64, name, *colour, body));
    }
    (party, looks)
}

/// The models themselves: the canon members come first and each is the canon character's OWN battle model (battle units,
/// its weapon, the very rig the kit makes for that character), and every player after them is its own FIELD body (the
/// field's scale, no weapon, no battle clips of its own: it moves procedurally), no player's model is any canon
/// member's and the players' differ among themselves. A canon battle model on a player is a bug.
fn check_models(b: &RealBattle, kit: &mut ModelKit, case: &Case) {
    let members: Vec<Arc<ActorRig>> = b.scene.actors.iter().filter(|a| a.side == Side::Party).map(|a| Arc::clone(&a.rig)).collect();
    assert_eq!(members.len(), CANON.len() + case.players.len(), "{}: the canon pair, then every player", case.name);
    for (i, (_, character)) in CANON.iter().enumerate() {
        let rig = &members[i];
        assert_eq!(rig.scale, 1.0, "{}: member {i} is a battle model, in battle units", case.name);
        assert_eq!(rig.attachments.len(), 1, "{}: member {i} carries its weapon", case.name);
        let model = kit.source.party_member(&PartyLook::companion(ActorId(i as u8), *character, false));
        let canon = kit.rigs.resolve(&model, BoxSpec::for_party(0));
        assert!(Arc::ptr_eq(&rig.asset, &canon.asset), "{}: member {i} is canon character {character}'s own battle model", case.name);
    }
    for (i, rig) in members.iter().enumerate().skip(CANON.len()) {
        assert_eq!(rig.scale, model::FIELD_TO_BATTLE_SCALE, "{}: player {i} is a field body at the field's scale", case.name);
        assert!(rig.attachments.is_empty(), "{}: a field body carries no weapon", case.name);
        assert_eq!(rig.kit, Kit::none(), "{}: a field body has no battle clips of its own", case.name);
        // Every case here gives each player a body of its own, so no two members share a model.
        for (c, other) in members.iter().enumerate().filter(|(c, _)| *c != i) {
            assert!(!Arc::ptr_eq(&rig.asset, &other.asset), "{}: member {i} and member {c} are not the same model", case.name);
        }
    }
}

/// The colours land on the players' bodies and nowhere else: the walk of a frame is each party member's body then its
/// weapons, in party order, then the enemies'.
fn check_colours(b: &RealBattle, case: &Case, frame: &BattleFrame) {
    let mut expected = Vec::new();
    for (i, actor) in b.scene.actors.iter().enumerate().filter(|(_, a)| a.side == Side::Party) {
        let colour = i.checked_sub(CANON.len()).map(|p| case.players[p].3);
        expected.push(colour.map_or(Identity::NONE, Identity::of));
        expected.resize(expected.len() + actor.rig.attachments.len(), Identity::NONE);
    }
    assert_eq!(frame.identities[..expected.len()], expected[..], "{}: only the players' bodies carry a colour, each its own", case.name);
    assert!(frame.identities[expected.len()..].iter().all(|i| *i == Identity::NONE), "{}: the enemies keep their own", case.name);
}

/// The owner's cases over his own data, as numbers and as pictures through the real renderer: the canon Cloud and Barret
/// (companions: their own battle models) and then every player in its OWN body, in its colour, whatever stat curve it
/// fights with. For each case it checks the models (no canon battle model on any player, the canon pair as themselves, no
/// second Cloud and no second Barret), the colours (the players' bodies only) and the composition (everybody inside the
/// window, nobody hidden behind another, the bottom free for the menus), and writes `battle_canon_<case>_1920x1080.png`
/// (with the overlay, as the player sees it) and `battle_canon_<case>_closeups.png` (the party members at 2x, no overlay)
/// in `$FMV_SHOT_DIR`. Needs `FMV_FF7_DATA` with the real `battle.lgp`: the canon pair are stand-ins without it.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_canon_party_with_every_player_in_its_own_body() {
    let Some(real) = open_real() else { return };
    if real.battle_lgp.is_none() {
        eprintln!("skipped: this machine has no battle.lgp, so the canon pair would be stand-ins");
        return;
    }
    let Some(mut gpu) = Offscreen::new() else { return };
    let (w, h) = (1920u32, 1080u32);
    let mut kit = Some(real.kit());
    for case in &CASES {
        let (party, looks) = roster_of_case(case);
        let b = real_battle(&real, &mut kit, party, looks, 0xB11);
        check_models(&b, kit.as_mut().expect("the kit"), case);

        let view = b.scene.view(w, h);
        let frame = b.scene.frame(&view, false);
        check_colours(&b, case, &frame);

        // The composition: everybody on the screen, inside the side margins, the bottom free; the party's members
        // stand apart (no one hides another's body).
        let anchors = b.scene.screen_anchors(&view);
        let mut party_x = Vec::new();
        let windows_top = |w: u32, h: u32| {
            let l = crate::battle::panels::layout(w, h, CANON.len() + case.players.len());
            (l.enemy.y * l.s) as f32
        };
        for a in &anchors {
            let (feet, head) = (a.feet.expect("feet"), a.head.expect("head"));
            assert!(a.drawn, "{}: {a:?} is drawn", case.name);
            assert!(feet[0] > w as f32 * 0.04 && feet[0] < w as f32 * 0.96, "{}: {a:?} is inside the side margins", case.name);
            assert!(head[1] > 0.0 && feet[1] < windows_top(w, h), "{}: {a:?} is inside, above the bottom windows (the party panel has a row per member)", case.name);
            if a.side == Side::Party {
                party_x.push((feet[0], feet[1] - head[1]));
            }
        }
        eprintln!("{}: party feet x and height in pixels {:?}", case.name, party_x.iter().map(|(x, t)| (x.round() as i32, t.round() as i32)).collect::<Vec<_>>());
        for (i, (xi, ti)) in party_x.iter().enumerate() {
            for (xj, tj) in &party_x[i + 1..] {
                assert!((xi - xj).abs() > 0.3 * ti.min(*tj), "{}: two party members stand on top of each other ({xi} and {xj})", case.name);
            }
        }

        // The pictures, and in each of them everybody really is drawn where the scene says (a model the GPU did not
        // have ready would be a hole in the picture): the pixels along each figure differ from the stage drawn the same
        // way with nobody on it.
        let mut overlay = DrawList::new();
        draw_battle_overlay(&b.battle, &b.scene.head_anchors(&view), w, h, &mut overlay);
        let with_overlay = gpu.render_battle(w, h, &frame, &overlay);
        let bare = gpu.render_battle(w, h, &frame, &DrawList::new());
        let mut empty_stage = b.scene.frame(&view, false);
        empty_stage.models.models.clear();
        empty_stage.models.ground.clear();
        let stage_with_overlay = gpu.render_battle(w, h, &empty_stage, &overlay);
        let stage_bare = gpu.render_battle(w, h, &empty_stage, &DrawList::new());
        for a in &anchors {
            assert!(drawn_at(&with_overlay, &stage_with_overlay, w, a), "{}: {a:?} is in the picture with the overlay", case.name);
            assert!(drawn_at(&bare, &stage_bare, w, a), "{}: {a:?} is in the picture without it", case.name);
        }
        save(&format!("battle_canon_{}_1920x1080", case.name), w, h, &with_overlay);
        // The most members the battle has (five) at the window sizes the window and the other pictures use: everybody on
        // the screen and above the bottom windows (the party panel has five rows here).
        if case.name == "three_players" {
            for (w2, h2) in [(1100u32, 680u32), (640, 480)] {
                let view2 = b.scene.view(w2, h2);
                let frame2 = b.scene.frame(&view2, false);
                let mut overlay2 = DrawList::new();
                draw_battle_overlay(&b.battle, &b.scene.head_anchors(&view2), w2, h2, &mut overlay2);
                for a in &b.scene.screen_anchors(&view2) {
                    let (feet, head) = (a.feet.expect("feet"), a.head.expect("head"));
                    assert!(a.drawn && feet[0] > w2 as f32 * 0.02 && feet[0] < w2 as f32 * 0.98, "{w2}x{h2}: {a:?} is inside the side margins");
                    assert!(head[1] > 0.0 && feet[1] < windows_top(w2, h2), "{w2}x{h2}: {a:?} is inside, above the bottom windows");
                }
                save(&format!("battle_canon_{}_{w2}x{h2}", case.name), w2, h2, &gpu.render_battle(w2, h2, &frame2, &overlay2));
            }
        }
        let closeups: Vec<Img> = anchors
            .iter()
            .filter(|a| a.side == Side::Party)
            .map(|a| {
                let (feet, head) = (a.feet.unwrap(), a.head.unwrap());
                let tall = feet[1] - head[1];
                around(&bare, w, h, (feet[0] - 0.55 * tall, head[1], feet[0] + 0.55 * tall, feet[1]), 14.0, 2)
            })
            .collect();
        let sheet = hstack(&closeups);
        save(&format!("battle_canon_{}_closeups", case.name), sheet.w, sheet.h, &sheet.px);
    }
}
