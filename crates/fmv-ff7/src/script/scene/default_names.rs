//! Explicit port policy for the original reactor-gate naming menus: keep the canonical
//! Cloud/Barret names already used by dialogue substitution, and perform native FIELD_Init
//! party synchronization. This does not implement a name editor or authorize other menus.
//! ScenePlay enables it only for v3; legacy presentation retains its ignored-menu behavior.
//!
//! Capability is restricted to the reviewed Steam English md1_1 section and the team-talk
//! execution definition. Modified/localized sections require their own review. The hash is
//! metadata only; no original script bytes or dialogue are included here.

use super::data::{team_talk_scene, SceneDef};
use crate::script::FieldScript;

pub(super) const POLICY: &[u8] = b"default-canonical-names\0v1\0";
const SECTION_HASH: [u8; 32] = [
    0xf8, 0xa8, 0x9b, 0x17, 0x2b, 0x28, 0xf3, 0xb6,
    0xd7, 0x7b, 0xb1, 0x8a, 0x30, 0xf6, 0x3c, 0x57,
    0x86, 0x36, 0x96, 0x90, 0x25, 0xe3, 0xbe, 0xe4,
    0x67, 0x89, 0xcd, 0x7c, 0x6c, 0x9c, 0x95, 0x4b,
];
// Entity, script slot, section-relative instruction, canonical character id.
const SITES: [(usize, usize, usize, u8); 2] = [(3, 4, 0x05A2, 0), (2, 4, 0x0516, 1)];

/// VM and replay identity use this same gate. A stable id alone grants no capability.
pub(super) fn sites(section: &[u8], def: &SceneDef) -> Vec<(usize, u8)> {
    if !definition_matches(def) || blake3::hash(section).as_bytes() != &SECTION_HASH {
        return Vec::new();
    }
    let Ok(script) = FieldScript::parse(section) else { return Vec::new() };
    let verified = SITES.iter().all(|&(entity, slot, offset, character)| {
        script.disassemble(entity, slot).ok().is_some_and(|instructions| instructions.iter().any(|ins|
            ins.offset == offset && ins.opcode == 0x49 && ins.len == 4 && !ins.stray_banks
                && ins.literal("type") == Some(6) && ins.literal("param") == Some(i32::from(character))))
    });
    if verified { SITES.iter().map(|&(_, _, offset, character)| (offset, character)).collect() }
    else { Vec::new() }
}

fn definition_matches(def: &SceneDef) -> bool {
    let expected = team_talk_scene();
    def.id == expected.id && def.game == expected.game && def.field == expected.field
        && def.trigger.entity == expected.trigger.entity && def.trigger.slot == expected.trigger.slot
        && def.trigger.talk_radius == expected.trigger.talk_radius && def.trigger.moments == expected.trigger.moments
        && def.setup == expected.setup && def.given == expected.given && def.root == expected.root
        && def.presents == expected.presents && def.presents_bit == expected.presents_bit
        && def.resume_after_battle == expected.resume_after_battle && def.cast.len() == expected.cast.len()
        && def.cast.iter().zip(&expected.cast).all(|(actual, canon)| actual.entity == canon.entity
            // Accepted trigger coordinates/facing replace Cloud's offline seed. That pose
            // is independently bound by replay identity and validated by the host.
            && if canon.entity == 1 { actual.start.is_some() } else { actual.start == canon.start })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::data::{Given, ScriptRef, Spot, Step};

    #[test]
    fn only_reviewed_execution_inputs_can_receive_the_policy() {
        let original = team_talk_scene();
        assert!(definition_matches(&original));
        let changes: &[fn(&mut SceneDef)] = &[
            |d| d.id.push_str("-other"), |d| d.field.push('x'), |d| d.game.push('x'),
            |d| d.trigger.entity = 1, |d| d.trigger.slot = 1, |d| d.trigger.talk_radius = 1,
            |d| d.trigger.moments = 0..=7, |d| d.setup.reverse(),
            |d| d.setup.push(ScriptRef::new(0, 1)),
            |d| d.given.push(Given { bank: 3, address: 9, value: 0 }),
            |d| d.root = vec![Step::Start(ScriptRef::new(0, 0))],
            |d| d.presents = None, |d| d.presents_bit = Some((3, 1, 0)),
            |d| d.resume_after_battle = Some(300), |d| d.cast.reverse(),
            |d| { d.cast.pop(); },
            |d| { d.cast.first_mut().unwrap().start = None; },
            |d| { d.cast.get_mut(1).unwrap().start = Some(Spot { x: 0, y: 0, triangle: 0, direction: 0 }); },
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut changed = original.clone();
            change(&mut changed);
            assert!(!definition_matches(&changed), "execution mutation {index}");
        }
        let mut posed = original;
        posed.cast.first_mut().unwrap().start = Some(Spot { x: 1141, y: 1460, triangle: 62, direction: 64 });
        posed.cast.first_mut().unwrap().name = "presentation label".into();
        posed.trigger.name.clear();
        posed.speakers.clear();
        posed.witness.clear();
        assert!(definition_matches(&posed), "accepted origin and presentation do not strip policy");
        assert!(sites(&[], &posed).is_empty(), "an id and matching definition never authorize unreviewed bytes");
    }

    #[test]
    #[ignore = "needs owner's FMV_FF7_DATA; verifies hashes and opcode metadata without writing game data"]
    fn owned_original_section_has_exactly_the_two_reviewed_naming_sites() {
        let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
        let mut archive = crate::lgp::LgpArchive::open_path(data.join("field/flevel.lgp")).unwrap();
        let bytes = crate::lzss::decompress(&archive.read("md1_1").unwrap()).unwrap();
        let field = crate::field::FieldFile::parse(bytes).unwrap();
        let script = FieldScript::from_field(&field).unwrap();
        let mut def = team_talk_scene();
        assert_eq!(sites(script.bytes(), &def), [(0x05A2, 0), (0x0516, 1)]);
        // Stop at the sixth window, immediately after Cloud's naming request and before
        // the later doors. This separately verifies v3 capability installation and legacy
        // presentation behavior without depending on animated-offset support.
        for extended in [false, true] {
            let mut story = if extended { crate::story::StoryState::fresh_first_control() }
                else { crate::story::StoryState::at(6) };
            story.moment = 6;
            let mut play = super::super::ScenePlay::with_resources(&field, &def, &story, 7,
                super::super::SceneResources { gil: Some(617) }).unwrap();
            play.next_strict(None).unwrap();
            for _ in 0..5 {
                let super::super::SceneBoundary::Window(mark) = play.snapshot().unwrap().boundary
                    else { panic!("team talk ended before the first naming request") };
                play.next_strict(Some(super::super::WindowAck { checkpoint: mark, answer: None })).unwrap();
            }
            let snapshot = play.snapshot().unwrap();
            assert!(matches!(snapshot.boundary, super::super::SceneBoundary::Window(mark) if mark.ordinal == 6));
            assert_eq!(snapshot.stats.handled.get("MENU").copied().unwrap_or(0), u32::from(extended));
            assert_eq!(snapshot.stats.ignored.get("MENU").copied().unwrap_or(0), u32::from(!extended));
            assert_eq!(snapshot.stats.unresolved, 0);
        }
        let unbound = super::super::replay_identity::fingerprint(script.bytes(), &def);
        def.cast.first_mut().unwrap().start.as_mut().unwrap().direction = 64;
        assert_eq!(sites(script.bytes(), &def), [(0x05A2, 0), (0x0516, 1)]);
        assert_ne!(unbound, super::super::replay_identity::fingerprint(script.bytes(), &def), "the actual origin remains bound");
        let mut changed = script.bytes().to_vec();
        *changed.last_mut().unwrap() ^= 1;
        assert!(sites(&changed, &def).is_empty(), "even non-code section changes need review");
        def.root.push(Step::Start(ScriptRef::new(3, 4)));
        assert!(sites(script.bytes(), &def).is_empty(), "the same original opcode cannot be authorized under another execution root");
    }
}
