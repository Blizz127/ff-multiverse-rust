//! Stable identity of the code and scene definition used to reconstruct a battle checkpoint.
//!
//! This binds the script section byte for byte, including its dialog table, and the definition
//! inputs that select code, seed variables, or establish actor positions. Labels, appearance,
//! speakers, witnesses and the debug arrival do not affect VM execution and are excluded.
//! The replay's story, seed, resources and accepted transcript must be bound separately by its
//! host. Bump the domain version if this encoding or the interpreter's replay contract changes.

use super::data::{SceneDef, ScriptRef, Spot, Step};
use crate::script::FieldScratch;

const DOMAIN: &[u8] = b"fmv-ff7/scene-replay-identity\0v1\0";

/// Opt-in checked visits bind their original values and knowledge independently of source code.
/// Absence delegates byte-for-byte to the legacy identity; no deployed replay changes meaning.
pub(super) fn fingerprint_with_scratch(section: &[u8], def: &SceneDef, scratch: Option<&FieldScratch>) -> [u8; 32] {
    let source = fingerprint(section, def);
    let Some(scratch) = scratch else { return source };
    let mut hash = blake3::Hasher::new();
    hash.update(b"fmv-ff7/scene-checked-scratch\0v1\0");
    hash.update(&source);
    hash.update(scratch.values());
    hash.update(scratch.known_masks());
    *hash.finalize().as_bytes()
}

/// The exact script and semantic definition a resumable replay was constructed with. Integers
/// are little-endian; strings, raw bytes and lists have u64 lengths; variants have explicit tags.
pub(super) fn fingerprint(section: &[u8], def: &SceneDef) -> [u8; 32] {
    let mut out = Encoding(blake3::Hasher::new());
    out.0.update(DOMAIN);
    out.0.update(&super::play::REPLAY_VERSION.to_le_bytes());
    out.bytes(section);
    out.bytes(def.id.as_bytes());
    out.bytes(def.game.as_bytes());
    out.bytes(def.field.as_bytes());
    out.byte(def.trigger.entity);
    out.byte(def.trigger.slot);
    out.word(def.trigger.talk_radius);
    out.word(*def.trigger.moments.start());
    out.word(*def.trigger.moments.end());
    out.byte(u8::from(def.requires_exact_transaction()));
    match def.line_trigger() {
        Some(line) => {
            out.byte(1);
            for coordinate in line.a.into_iter().chain(line.b) { out.0.update(&coordinate.to_le_bytes()); }
        }
        None => out.byte(0),
    }
    out.len(def.setup.len());
    for reference in &def.setup { out.script(*reference); }
    out.len(def.given.len());
    for given in &def.given { out.0.update(&[given.bank, given.address, given.value]); }
    out.len(def.root.len());
    for step in &def.root {
        match *step {
            Step::SetBit { bank, address, bit } => { out.0.update(&[0, bank, address, bit]); }
            Step::Await(reference) => { out.byte(1); out.script(reference); }
            Step::Start(reference) => { out.byte(2); out.script(reference); }
            Step::StartMain(entity) => { out.0.update(&[3, entity]); }
            Step::StartAmbientMain(entity) => { out.0.update(&[4, entity]); }
        }
    }
    out.len(def.cast.len());
    for member in &def.cast {
        out.byte(member.entity);
        match &member.start {
            Some(spot) => { out.byte(1); out.spot(spot); }
            None => out.byte(0),
        }
    }
    out.optional_word(def.presents);
    match def.presents_bit {
        Some((bank, address, bit)) => { out.0.update(&[1, bank, address, bit]); }
        None => out.byte(0),
    }
    out.optional_word(def.resume_after_battle);
    let name_sites = super::default_names::sites(section, def);
    if !name_sites.is_empty() {
        // V3 playback authorizes this policy; legacy playback still ignores these menus.
        // The host separately binds the story encoding. A v3 replay cannot be reconstructed
        // as an older ignored MENU run. The exact original bytes are already bound above.
        out.bytes(super::default_names::POLICY);
        out.len(name_sites.len());
        for (offset, character) in name_sites { out.len(offset); out.byte(character); }
    }
    *out.0.finalize().as_bytes()
}

struct Encoding(blake3::Hasher);

impl Encoding {
    fn byte(&mut self, value: u8) { self.0.update(&[value]); }
    fn word(&mut self, value: u16) { self.0.update(&value.to_le_bytes()); }
    fn len(&mut self, value: usize) { self.0.update(&(value as u64).to_le_bytes()); }
    fn bytes(&mut self, bytes: &[u8]) { self.len(bytes.len()); self.0.update(bytes); }
    fn script(&mut self, reference: ScriptRef) { self.0.update(&[reference.entity, reference.slot]); }
    fn spot(&mut self, spot: &Spot) {
        self.0.update(&spot.x.to_le_bytes());
        self.0.update(&spot.y.to_le_bytes());
        self.word(spot.triangle);
        self.byte(spot.direction);
    }
    fn optional_word(&mut self, value: Option<u16>) {
        match value {
            Some(value) => { self.byte(1); self.word(value); }
            None => self.byte(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::scene::data::{CastMember, Given, Look, Trigger};

    fn definition() -> SceneDef {
        SceneDef {
            id: "ff7/test/replay".into(), game: "ff7".into(), field: "test".into(),
            trigger: Trigger { entity: 2, name: "speaker".into(), slot: 1, talk_radius: 32, moments: 9..=12 },
            setup: vec![ScriptRef::new(2, 0), ScriptRef::new(3, 0)],
            given: vec![Given { bank: 5, address: 20, value: 8 }, Given { bank: 5, address: 21, value: 3 }],
            root: vec![Step::SetBit { bank: 3, address: 42, bit: 2 }, Step::Await(ScriptRef::new(2, 1)), Step::Start(ScriptRef::new(3, 1)), Step::StartMain(4)],
            cast: vec![CastMember { entity: 2, name: "speaker".into(), look: Look::Appearance("ff7/cloud".into()), start: Some(Spot { x: -5, y: 10, triangle: 3, direction: 128 }) }],
            speakers: vec![("Cloud".into(), 2)], witness: vec![],
            arrival: Spot { x: 20, y: 30, triangle: 5, direction: 0 },
            presents: Some(12), presents_bit: Some((3, 42, 2)), resume_after_battle: Some(300),
        }
    }

    #[test]
    fn every_execution_input_and_its_order_changes_the_identity() {
        let original = definition();
        let code = [0, 0x70, 0x2C, 1];
        let expected = fingerprint(&code, &original);
        assert_eq!(expected, fingerprint(&code, &original.clone()));
        assert_ne!(expected, fingerprint(&[0, 0x70, 0x2D, 1], &original));
        let mutations: &[fn(&mut SceneDef)] = &[
            |d| d.id.push('x'), |d| d.game.push('x'), |d| d.field.push('x'),
            |d| d.trigger.entity += 1, |d| d.trigger.slot += 1, |d| d.trigger.talk_radius += 1,
            |d| d.trigger.moments = 8..=12, |d| d.trigger.moments = 9..=13,
            |d| d.setup[0].entity += 1, |d| d.setup[0].slot += 1, |d| d.setup.reverse(),
            |d| d.given[0].bank += 1, |d| d.given[0].address += 1, |d| d.given[0].value += 1, |d| d.given.reverse(),
            |d| d.root[0] = Step::SetBit { bank: 4, address: 42, bit: 2 },
            |d| d.root[0] = Step::SetBit { bank: 3, address: 43, bit: 2 },
            |d| d.root[0] = Step::SetBit { bank: 3, address: 42, bit: 3 },
            |d| d.root[1] = Step::Start(ScriptRef::new(2, 1)),
            |d| d.root[2] = Step::Start(ScriptRef::new(3, 2)),
            |d| d.root[3] = Step::StartMain(5), |d| d.root.reverse(),
            |d| d.root[3] = Step::StartAmbientMain(4),
            |d| d.cast[0].entity += 1,
            |d| d.cast[0].start.as_mut().unwrap().x += 1,
            |d| d.cast[0].start.as_mut().unwrap().y += 1,
            |d| d.cast[0].start.as_mut().unwrap().triangle += 1,
            |d| d.cast[0].start.as_mut().unwrap().direction += 1,
            |d| d.cast[0].start = None,
            |d| d.presents = None, |d| d.presents = Some(13),
            |d| d.presents_bit = None, |d| d.presents_bit = Some((3, 43, 2)),
            |d| d.resume_after_battle = None, |d| d.resume_after_battle = Some(301),
        ];
        for (index, change) in mutations.iter().enumerate() {
            let mut changed = original.clone();
            change(&mut changed);
            assert_ne!(expected, fingerprint(&code, &changed), "semantic mutation {index}");
        }
    }

    #[test]
    fn presentation_changes_do_not_change_replay_identity() {
        let original = definition();
        let mut changed = original.clone();
        changed.trigger.name = "another label".into();
        changed.cast[0].name = "another cast label".into();
        changed.cast[0].look = Look::Model { hrc: "different".into(), idle: "other".into() };
        changed.speakers = vec![("Another speaker".into(), 2)];
        changed.witness.push(Spot { x: 99, y: 88, triangle: 7, direction: 6 });
        changed.arrival = Spot { x: 77, y: 66, triangle: 5, direction: 4 };
        assert_eq!(fingerprint(&[0, 1, 2], &original), fingerprint(&[0, 1, 2], &changed));
    }

    #[test]
    fn variable_boundaries_and_optional_tags_are_unambiguous() {
        let mut a = definition();
        let mut b = a.clone();
        a.id = "a".into(); a.game = "bc".into();
        b.id = "ab".into(); b.game = "c".into();
        assert_ne!(fingerprint(&[], &a), fingerprint(&[], &b));
        a = definition(); b = a.clone();
        a.presents = None; a.resume_after_battle = Some(0);
        b.presents = Some(0); b.resume_after_battle = None;
        assert_ne!(fingerprint(&[], &a), fingerprint(&[], &b));
        assert_ne!(fingerprint(&[0], &a), fingerprint(&[], &a));
    }
}
