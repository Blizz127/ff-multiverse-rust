//! The two station guards at the original moment-1 triangle trigger.
//!
//! These describe presentations of the existing station story battle. They do not run the
//! earlier movie/opening, initialize a new game, save the moment or award the fight.
//! The original director is entered normally; its GETAI must see the accepted player
//! position, so Cloud has no invented start position in either definition.

use super::*;

pub const STATION_GUARD_SCENE_ID: &str = "ff7/md1stin/station-guards";
pub const STATION_GUARD_VICTORY_SCENE_ID: &str = "ff7/md1stin/station-guards-victory";

/// The station's original guard approach before battle 300. Director entity 0's main
/// checks Cloud's triangle against the initializer's scratch word 6:9 (12), then checks
/// moment 1. It writes moment 6, locks controls, starts soldier 4's slot 3 and awaits
/// soldier 5's slot 3 before BATTLE 300. Soldier 4's independently running tail remains
/// part of that same script execution; its timing is not replaced by an extra wait.
///
/// The world validates the existing story point before starting this presentation and
/// retains its normal victory/save ordering. Bind the accepted crossing with
/// [`crate::script::scene::with_trigger_position`] before constructing playback.
pub fn station_guard_scene() -> SceneDef {
    let cast = [(1, "cl", "aaaa"), (4, "hei0", "bwab"), (5, "hei1", "bwab")]
        .into_iter().map(|(entity, name, hrc)| CastMember {
            entity, name: name.into(),
            look: Look::Model { hrc: hrc.into(), idle: "acfe".into() },
            start: None,
        }).collect();
    SceneDef {
        id: STATION_GUARD_SCENE_ID.into(), game: "ff7".into(), field: "md1stin".into(),
        trigger: Trigger { entity: 0, name: "dir".into(), slot: 0, talk_radius: 0, moments: 1..=1 },
        // Director init establishes the actual triangle predicate and battle mode. The
        // soldiers' inits place and hide them; Cloud's init identifies the controlled actor.
        setup: [0, 1, 4, 5].into_iter().map(|entity| ScriptRef::new(entity, 0)).collect(),
        given: vec![], root: vec![Step::StartMain(0)], cast,
        speakers: vec![], witness: vec![],
        // Offline preview only: rounded interior centroid of original walkmesh triangle
        // 12. Facing zero is a preview convention; the live trigger supplies its heading.
        arrival: Spot { x: 3783, y: 28446, triangle: 12, direction: 0 },
        presents: Some(6), presents_bit: None, resume_after_battle: None,
    }
}

/// Unregistered survey of the director after a matching story victory. Its asynchronous
/// slot-4 request hides soldier 5, then it immediately releases controls and returns to
/// its moment-guarded polling loop. Soldier 4 may still have work pending from the approach.
/// Registering this needs a presentation lifecycle that returns control while those actor
/// jobs continue. Waiting for them would add an unauthored control hold; treating the
/// conditional poll as a RET would conceal that missing lifecycle. The existing ordinary
/// battle completion/save path remains responsible for the live victory return.
pub fn station_guard_victory_scene() -> SceneDef {
    let mut def = station_guard_scene();
    def.id = STATION_GUARD_VICTORY_SCENE_ID.into();
    def.resume_after_battle = Some(300);
    def
}

pub(super) fn is_station_guard(id: &str) -> bool {
    matches!(id, STATION_GUARD_SCENE_ID | STATION_GUARD_VICTORY_SCENE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_guard_presentations_keep_the_existing_extended_battle_boundary() {
        let before = StoryState::at(1).into_extended().unwrap();
        let after = StoryState::at(6).into_extended().unwrap();
        let prelude = station_guard_scene();
        let victory = station_guard_victory_scene();
        assert!(prelude.presents_step("md1stin", &before, &after));
        assert!(!victory.presents_step("md1stin", &before, &after));
        assert!(victory.follows_battle_step("md1stin", &before, &after, 300));
        assert!(!victory.follows_battle_step("md1stin", &before, &after, 303));
        for def in [&prelude, &victory] {
            assert!(!def.starts_on_arrival() && !def.offered_to(&before));
            assert!(!def.presents_step("md1stin", &StoryState::at(1), &StoryState::at(6)));
            assert!(!def.follows_battle_step("md1stin", &StoryState::at(1), &StoryState::at(6), 300));
            assert!(!def.applies_at(0) && !def.applies_at(6));
            assert_eq!(def.origin_actor(), Some(1));
            assert!(def.requires_presentation_origin());
            assert_eq!(def.presentation_player_actor(), Some(1));
            assert!(def.member(1).unwrap().start.is_none(), "the accepted position is an input, not a cast spawn");
            assert!(!def.requires_continuation() && !def.requires_exact_transaction());
        }
    }
}
