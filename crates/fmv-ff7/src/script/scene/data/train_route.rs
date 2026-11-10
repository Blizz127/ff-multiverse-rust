//! The original passenger-train lesson, return and departure at moment 51.
//!
//! These definitions are not registered. They retain both Jessie answer paths and both
//! rootmap producer branches. Production needs field-visit scratch state, durable landing
//! routing, the authored movie boundary, and rootmap palette/model-offset presentation.
//! Persistent inputs are always read from the supplied story; none is supplied by `Given`.

use super::*;

pub const JESSIE_TRAIN_TALK_SCENE_ID: &str = "ff7/tin_1/jessie-train-talk";
pub const TRAIN_MAP_SCENE_ID: &str = "ff7/rootmap/train-map";
pub const TRAIN_MAP_RETURN_SCENE_ID: &str = "ff7/tin_1/train-map-return";
pub const TRAIN_DEPARTURE_SCENE_ID: &str = "ff7/tin_1/sector-seven-departure";

/// The shared carriage background mains at moment 51. Wedge's standing animation also
/// loops; it must not keep a finite talk alive. Other actors' moment-51 mains place them
/// from their own scripts, including Biggs's independent bit-0/bit-2 facing conditions.
fn train_field_roots() -> Vec<Step> {
    [9, 10, 11, 12, 16, 20, 35].into_iter().map(Step::StartAmbientMain)
        .chain([31, 33, 34, 37].into_iter().map(Step::StartMain)).collect()
}

fn train_route(id: &str) -> SceneDef {
    let mut def = train_scene();
    def.id = id.into();
    def.trigger.moments = 51..=51;
    def.root = train_field_roots();
    def
}

/// Jessie's actual talk slot, including the optional later affection question.
///
/// `talked_this_visit` is the retained field-local byte 5:31, not a persistent story flag:
/// the native engine clears scratch on field entry, then the first repeat talk sets it.
/// A production caller must supply the current visit's value and retain the slot's new
/// value after completion; always passing false would allow the affection deduction again.
/// The accepted player position must be bound before playback (`with_trigger_position`).
/// Biggs's main additionally requires known bit 0 of bank 3:223 for his placement/facing.
pub fn jessie_train_talk_scene(talked_this_visit: bool) -> SceneDef {
    let mut def = train_route(JESSIE_TRAIN_TALK_SCENE_ID);
    def.trigger = Trigger { entity: 34, name: "avaw".into(), slot: 1, talk_radius: 80, moments: 51..=51 };
    // Jessie has no TALKR override. The original field initializer sets TalkRange to
    // 80 * field_scale / 512 (ff7-decomp d4db3de3, field_event.c:195).
    if let Some(jessie) = def.cast.iter_mut().find(|member| member.entity == 34) {
        // Her moment-51 main's XYZI/DIR, used to announce the talk before that main plays.
        jessie.start = Some(Spot { x: 110, y: -337, triangle: 55, direction: 96 });
    }
    def.given.push(Given { bank: 5, address: 31, value: u8::from(talked_this_visit) });
    def.root.push(Step::Await(ScriptRef::new(34, 1)));
    // This is an offline preview position only; production binds the actual nearby player.
    def.arrival = Spot { x: 72, y: -319, triangle: 47, direction: 64 };
    def
}

/// The original rootmap producer and director. With known bit 4 clear the eight-window
/// lesson returns to the train; with it set the producer selects movie 24 and the station.
/// The two field models are the original close-up rigs, not ordinary train bodies.
/// Their OFST and palette operations must be rendered before this can be registered.
pub fn train_map_scene() -> SceneDef {
    SceneDef {
        id: TRAIN_MAP_SCENE_ID.into(), game: "ff7".into(), field: "rootmap".into(),
        trigger: Trigger { entity: 4, name: "produce".into(), slot: 0, talk_radius: 0, moments: 51..=51 },
        setup: (0..14).map(|entity| ScriptRef::new(entity, 0)).collect(), given: vec![],
        root: vec![Step::StartAmbientMain(2), Step::StartAmbientMain(3), Step::StartAmbientMain(8),
            Step::StartMain(4), Step::StartMain(5)],
        cast: [(12, "cloud", "clgd", "cmda"), (13, "avaw", "cmde", "cmhb")].into_iter()
            .map(|(entity, name, hrc, idle)| CastMember { entity, name: name.into(),
                look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None }).collect(),
        // The owned CMDE skeleton is a head/upper-body/ponytail rig. Its loader name
        // mentions Midgar, but that is not evidence to replace Jessie's close-up with a map.
        speakers: vec![("Cloud".into(), 12), ("Jessie".into(), 13)], witness: vec![],
        // Both original model slots place their base here, with DIR64 for Cloud. Their
        // XYZI carries triangle53, but ROOTMAP has only five triangles: the owned floor
        // contains these exact coordinates in triangle4. The incoming MAPJUMP's (0,0,0)
        // is a placeholder. Participants still remain hidden/held through the close-ups.
        arrival: Spot { x: -18, y: -459, triangle: 4, direction: 64 },
        presents: None, presents_bit: None, resume_after_battle: None,
    }
}

/// The train's moment-51 entry after the map lesson. A new field visit clears temporary
/// bank 5, then Cloud's main, director 17 and its requested Jessie work share that one VM.
/// In particular director 19:1 waits for Jessie 34:7 to set scratch 5:30 before releasing
/// control. This is a landing-only definition, not something to restart on every login.
pub fn train_map_return_scene() -> SceneDef {
    let mut def = train_route(TRAIN_MAP_RETURN_SCENE_ID);
    def.root.extend([Step::StartMain(30), Step::StartMain(17)]);
    def.arrival = Spot { x: 39, y: -325, triangle: 49, direction: 28 };
    def
}

/// The original border2 crossing, including all ten windows and movie 24. The main of
/// border2 disables this line while bank 3:223 bit 2 is clear; its trigger slot does not
/// repeat that test. `SceneDef::offered_to` therefore requires the bit explicitly known set.
/// The actual crossing must be bound to Cloud before playback.
pub fn train_departure_scene() -> SceneDef {
    let mut def = train_route(TRAIN_DEPARTURE_SCENE_ID);
    def.trigger = Trigger { entity: 27, name: "border2".into(), slot: 2, talk_radius: 0, moments: 51..=51 };
    def.root.push(Step::Await(ScriptRef::new(27, 2)));
    def.arrival = Spot { x: -1, y: -215, triangle: 43, direction: 0 };
    def
}

/// Unknown persistent guards are not field-entry defaults. These fences supplement the
/// VM's unknown-read audit; they do not establish every rendering/runtime prerequisite.
pub(super) fn route_inputs_known(def: &SceneDef, story: &StoryState) -> bool {
    match def.id.as_str() {
        JESSIE_TRAIN_TALK_SCENE_ID => story.known_bit(3, 223, 2).is_some(),
        TRAIN_MAP_SCENE_ID => story.known_bit(3, 223, 4).is_some(),
        TRAIN_MAP_RETURN_SCENE_ID | TRAIN_DEPARTURE_SCENE_ID => story.known_bit(3, 223, 2) == Some(true),
        _ => true,
    }
}
