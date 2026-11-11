//! A bounded checkpoint of an actual, settled terminal cast, including exact presentation clocks.
//!
//! Version one is for the passenger-train arrival's nine actors. It deliberately refuses active
//! movement, turning and OFST interpolation. A replay supplies the authored structure and the
//! background journal; the checkpoint may supply only the recorded animation/effect phase. It
//! cannot inject a different logical position, model clip, visibility, camera instruction or
//! default animation. Effect validation is train-specific: SHAKE configuration must match and
//! only FADE's fractional clock may differ (this arrival has no authored FADE). A future profile
//! with an advancing fade needs a separately reviewed validator. This is presentation state,
//! never an authoritative save or scene outcome.

use super::*;

const MAGIC: &[u8; 4] = b"FSTC";
const VERSION: u16 = 1;
const MAX_ACTORS: usize = 9;
const MAX_CLIP_BYTES: usize = 256;
const FADE_BYTES: usize = 18;
const SHAKE_BYTES: usize = 34;
// Header, nine maximally populated actors, the largest camera instruction and effects.
// An actor has 32 fixed bytes, two optional animations and one optional standing default.
/// The exact maximum v1 encoding: 7,630 bytes, below 8 KiB even with an 84-byte envelope.
pub const MAX_CAST_CHECKPOINT_BYTES: usize = 7 + MAX_ACTORS * (32
    + 2 * (1 + 2 + MAX_CLIP_BYTES + 4 + 1 + 1 + 8)
    + (1 + 1 + 2 + MAX_CLIP_BYTES)) + 9 + 4 + 16 + FADE_BYTES + SHAKE_BYTES;

/// Exact terminal state captured before `Cast::clear`. The background journal is deliberately
/// absent: restoration takes it from a separately validated replay, against a fresh background.
#[derive(Debug, Clone)]
pub struct CastCheckpoint {
    cast: Cast,
}

impl PartialEq for CastCheckpoint {
    fn eq(&self, other: &Self) -> bool { self.encoded() == other.encoded() }
}
impl Eq for CastCheckpoint {}

impl Cast {
    /// Capture the actual terminal clocks and effects. This narrow format accepts at most nine
    /// actors and no unfinished movement; it does not silently settle or round any state.
    pub fn checkpoint(&self) -> Result<CastCheckpoint, String> {
        validate_cast(self)?;
        let mut cast = self.clone();
        cast.background_ops.clear();
        Ok(CastCheckpoint { cast })
    }
}

impl CastCheckpoint {
    /// Read-only decoded presentation state; syntax validation alone grants no source authority.
    pub fn cast(&self) -> &Cast { &self.cast }

    /// Versioned little-endian bytes. Float fields use their exact IEEE bits, including -0.
    pub fn encode(&self) -> Vec<u8> { self.encoded() }

    /// Read one complete bounded checkpoint. Truncation, trailing bytes and unknown versions
    /// fail before it can be restored; no collection size comes from an unchecked wire value.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_CAST_CHECKPOINT_BYTES { return Err("terminal cast checkpoint exceeds its byte bound".into()); }
        let mut input = Reader { bytes, at: 0 };
        if input.take(4)? != MAGIC || input.u16()? != VERSION { return Err("unsupported terminal cast checkpoint".into()); }
        let count = usize::from(input.u8()?);
        if count > MAX_ACTORS { return Err("terminal cast checkpoint has too many actors".into()); }
        let mut actors = BTreeMap::new();
        let mut previous = None;
        for _ in 0..count {
            let actor = input.actor()?;
            if previous.is_some_and(|id| id >= actor.entity) { return Err("terminal cast entities are not unique and ordered".into()); }
            previous = Some(actor.entity);
            actors.insert(actor.entity, actor);
        }
        let camera = match input.u8()? {
            0 => CameraState::Default,
            1 => CameraState::Scroll { x: input.i16()?, y: input.i16()?, frames: input.u32()? },
            2 => CameraState::Follow { entity: input.u8()? },
            _ => return Err("invalid terminal camera kind".into()),
        };
        let camera_frames = input.f32()?;
        let counts = Counts { anims_skipped: input.u32()?, moves_skipped: input.u32()?, camera_ops: input.u32()?, off_cast: input.u32()? };
        let fade = FadeState::from_checkpoint_bytes(input.take(FADE_BYTES)?)?;
        let shake = ShakeState::from_checkpoint_bytes(input.take(SHAKE_BYTES)?)?;
        if input.at != bytes.len() { return Err("trailing terminal cast checkpoint bytes".into()); }
        let cast = Cast { actors, camera, camera_frames, background_ops: Vec::new(), fade, shake, counts };
        validate_cast(&cast)?;
        Ok(Self { cast })
    }

    /// Restore recorded phase only after the replay agrees on every authored actor descriptor.
    /// This does not prove historical timing: the captured checkpoint supplies that evidence.
    pub fn restore_against(&self, replay: &Cast) -> Result<Cast, String> {
        validate_cast(replay)?;
        if self.cast.camera != replay.camera || self.cast.counts != replay.counts
            || self.cast.actors.len() != replay.actors.len() || !same_effect_configuration(&self.cast, replay) {
            return Err("terminal cast checkpoint differs from replay structure".into());
        }
        for (id, recorded) in &self.cast.actors {
            let Some(expected) = replay.actors.get(id) else { return Err("terminal cast checkpoint has a different actor".into()) };
            if !same_actor_shape(recorded, expected) {
                return Err(format!("terminal cast actor {id} differs from replay structure"));
            }
        }
        let mut cast = self.cast.clone();
        cast.background_ops = replay.background_ops.clone();
        Ok(cast)
    }

    fn encoded(&self) -> Vec<u8> {
        let mut out = Writer(Vec::new());
        out.0.extend_from_slice(MAGIC);
        out.u16(VERSION);
        out.u8(self.cast.actors.len() as u8);
        for actor in self.cast.actors.values() { out.actor(actor); }
        match self.cast.camera {
            CameraState::Default => out.u8(0),
            CameraState::Scroll { x, y, frames } => { out.u8(1); out.i16(x); out.i16(y); out.u32(frames); }
            CameraState::Follow { entity } => { out.u8(2); out.u8(entity); }
        }
        out.f32(self.cast.camera_frames);
        for count in [self.cast.counts.anims_skipped, self.cast.counts.moves_skipped, self.cast.counts.camera_ops, self.cast.counts.off_cast] { out.u32(count); }
        out.0.extend(self.cast.fade.checkpoint_bytes());
        out.0.extend(self.cast.shake.checkpoint_bytes());
        out.0
    }
}

fn phase(value: f32) -> bool { value.is_finite() && value >= 0.0 }
fn same_float(a: f32, b: f32) -> bool { a.to_bits() == b.to_bits() }

fn same_effect_configuration(a: &Cast, b: &Cast) -> bool {
    // Train uses one constant authored amplitude/duration. The shared effect helper
    // checks both that configuration and the attainable segment range, retaining phase.
    let shake = a.shake.matches_constant_configuration(&b.shake);
    // Train's default fade can accumulate a fractional clock but has no authored fade
    // instruction. Refuse any changed mode, colour/history, speed or counter. This is
    // intentionally narrower than generic FADE/NFADE phase restoration.
    let (a_fade, b_fade) = (a.fade.checkpoint_bytes(), b.fade.checkpoint_bytes());
    shake && a_fade.get(..14).zip(b_fade.get(..14)).is_some_and(|(a, b)| a == b)
}

fn validate_cast(cast: &Cast) -> Result<(), String> {
    if cast.actors.len() > MAX_ACTORS { return Err("terminal cast checkpoint has too many actors".into()); }
    if !phase(cast.camera_frames) { return Err("invalid terminal camera clock".into()); }
    for (&id, actor) in &cast.actors {
        if id != actor.entity || actor.busy() { return Err("terminal cast checkpoint requires settled actors".into()); }
        if actor.pos.is_some_and(|p| p.iter().any(|n| !n.is_finite())) || !actor.heading.is_finite() || !phase(actor.walked_frames) {
            return Err("invalid terminal actor coordinates or clock".into());
        }
        for animation in [&actor.animation, &actor.movement_animation].into_iter().flatten() {
            check_clip(&animation.clip)?;
            if !matches!(animation.rate, 1.0 | 2.0) || !valid_frame(animation.frame) {
                return Err("invalid terminal animation clock or descriptor".into());
            }
        }
        if let Some(default) = &actor.default_animation { check_clip(&default.clip)?; }
    }
    // The effect types validate their own private invariants, including native counter wrap.
    FadeState::from_checkpoint_bytes(&cast.fade.checkpoint_bytes())?;
    ShakeState::from_checkpoint_bytes(&cast.shake.checkpoint_bytes())?;
    Ok(())
}

fn check_clip(clip: &str) -> Result<(), String> {
    if clip.is_empty() || clip.len() > MAX_CLIP_BYTES { Err("terminal animation name exceeds its bounds".into()) } else { Ok(()) }
}

fn valid_frame(frame: ClipFrame) -> bool {
    match frame {
        ClipFrame::Hold(_) => true,
        ClipFrame::Loop(elapsed) | ClipFrame::HoldFull { elapsed } | ClipFrame::OnceFull { elapsed }
        | ClipFrame::OnceFullDoubled { elapsed } => phase(elapsed),
        ClipFrame::HoldRange { first, last, elapsed } => first <= last && phase(elapsed),
        ClipFrame::OnceFullSlowed { elapsed, doubled_until } => phase(elapsed) && phase(doubled_until) && doubled_until <= elapsed,
    }
}

fn same_actor_shape(a: &Actor, b: &Actor) -> bool {
    let position = match (a.pos, b.pos) {
        (None, None) => true,
        (Some(a), Some(b)) => a.into_iter().zip(b).all(|(a, b)| same_float(a, b)),
        _ => false,
    };
    position && a.entity == b.entity && a.render_offset == b.render_offset && a.triangle == b.triangle
        && same_float(a.heading, b.heading) && a.visible == b.visible && a.default_animation == b.default_animation
        && same_animation_shape(a.animation.as_ref(), b.animation.as_ref())
        && same_animation_shape(a.movement_animation.as_ref(), b.movement_animation.as_ref())
}

fn same_animation_shape(a: Option<&SceneAnimation>, b: Option<&SceneAnimation>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.clip == b.clip && same_float(a.rate, b.rate) && a.once == b.once && match (a.frame, b.frame) {
            (ClipFrame::Hold(a), ClipFrame::Hold(b)) => a == b,
            (ClipFrame::HoldRange { first: af, last: al, .. }, ClipFrame::HoldRange { first: bf, last: bl, .. }) => af == bf && al == bl,
            (ClipFrame::Loop(_), ClipFrame::Loop(_)) | (ClipFrame::HoldFull { .. }, ClipFrame::HoldFull { .. })
            | (ClipFrame::OnceFull { .. }, ClipFrame::OnceFull { .. })
            | (ClipFrame::OnceFullDoubled { .. }, ClipFrame::OnceFullDoubled { .. })
            | (ClipFrame::OnceFullSlowed { .. }, ClipFrame::OnceFullSlowed { .. }) => true,
            _ => false,
        },
        _ => false,
    }
}

struct Writer(Vec<u8>);
impl Writer {
    fn u8(&mut self, value: u8) { self.0.push(value); }
    fn bool(&mut self, value: bool) { self.u8(u8::from(value)); }
    fn u16(&mut self, value: u16) { self.0.extend(value.to_le_bytes()); }
    fn i16(&mut self, value: i16) { self.0.extend(value.to_le_bytes()); }
    fn u32(&mut self, value: u32) { self.0.extend(value.to_le_bytes()); }
    fn f32(&mut self, value: f32) { self.u32(value.to_bits()); }
    fn clip(&mut self, clip: &str) { self.u16(clip.len() as u16); self.0.extend(clip.as_bytes()); }
    fn actor(&mut self, actor: &Actor) {
        self.u8(actor.entity);
        self.bool(actor.pos.is_some());
        if let Some(pos) = actor.pos { for value in pos { self.f32(value); } }
        for value in actor.render_offset { self.i16(value); }
        self.bool(actor.triangle.is_some());
        if let Some(triangle) = actor.triangle { self.u16(triangle); }
        self.f32(actor.heading);
        self.bool(actor.visible);
        self.f32(actor.walked_frames);
        self.animation(actor.animation.as_ref());
        self.animation(actor.movement_animation.as_ref());
        self.bool(actor.default_animation.is_some());
        if let Some(default) = &actor.default_animation { self.u8(default.slot); self.clip(&default.clip); }
    }
    fn animation(&mut self, animation: Option<&SceneAnimation>) {
        self.bool(animation.is_some());
        let Some(animation) = animation else { return };
        self.clip(&animation.clip);
        self.f32(animation.rate);
        self.bool(animation.once);
        match animation.frame {
            ClipFrame::Loop(frame) => { self.u8(0); self.f32(frame); }
            ClipFrame::Hold(frame) => { self.u8(1); self.u8(frame); }
            ClipFrame::HoldRange { first, last, elapsed } => { self.u8(2); self.u8(first); self.u8(last); self.f32(elapsed); }
            ClipFrame::HoldFull { elapsed } => { self.u8(3); self.f32(elapsed); }
            ClipFrame::OnceFull { elapsed } => { self.u8(4); self.f32(elapsed); }
            ClipFrame::OnceFullDoubled { elapsed } => { self.u8(5); self.f32(elapsed); }
            ClipFrame::OnceFullSlowed { elapsed, doubled_until } => { self.u8(6); self.f32(elapsed); self.f32(doubled_until); }
        }
    }
}

struct Reader<'a> { bytes: &'a [u8], at: usize }
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(count).ok_or("terminal cast checkpoint length overflow")?;
        let bytes = self.bytes.get(self.at..end).ok_or("truncated terminal cast checkpoint")?;
        self.at = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?.try_into().map_err(|_| "invalid terminal cast field width".into())
    }
    fn u8(&mut self) -> Result<u8, String> { let [byte] = self.array()?; Ok(byte) }
    fn bool(&mut self) -> Result<bool, String> { match self.u8()? { 0 => Ok(false), 1 => Ok(true), _ => Err("invalid terminal cast boolean".into()) } }
    fn u16(&mut self) -> Result<u16, String> { Ok(u16::from_le_bytes(self.array()?)) }
    fn i16(&mut self) -> Result<i16, String> { Ok(i16::from_le_bytes(self.array()?)) }
    fn u32(&mut self) -> Result<u32, String> { Ok(u32::from_le_bytes(self.array()?)) }
    fn f32(&mut self) -> Result<f32, String> { Ok(f32::from_bits(self.u32()?)) }
    fn clip(&mut self) -> Result<String, String> {
        let count = usize::from(self.u16()?);
        if count == 0 || count > MAX_CLIP_BYTES { return Err("terminal animation name exceeds its bounds".into()); }
        String::from_utf8(self.take(count)?.to_vec()).map_err(|_| "terminal animation name is not UTF-8".into())
    }
    fn actor(&mut self) -> Result<Actor, String> {
        let entity = self.u8()?;
        let pos = if self.bool()? { Some([self.f32()?, self.f32()?, self.f32()?]) } else { None };
        let render_offset = [self.i16()?, self.i16()?, self.i16()?];
        let triangle = if self.bool()? { Some(self.u16()?) } else { None };
        let heading = self.f32()?;
        let visible = self.bool()?;
        let walked_frames = self.f32()?;
        let animation = self.animation()?;
        let movement_animation = self.animation()?;
        let default_animation = if self.bool()? { Some(DefaultAnimation { slot: self.u8()?, clip: self.clip()? }) } else { None };
        Ok(Actor { entity, pos, render_offset, offset_motion: None, triangle, heading, visible, walk: None, turn: None,
            walked_frames, animation, movement_animation, default_animation })
    }
    fn animation(&mut self) -> Result<Option<SceneAnimation>, String> {
        if !self.bool()? { return Ok(None); }
        let clip = self.clip()?;
        let rate = self.f32()?;
        let once = self.bool()?;
        let frame = match self.u8()? {
            0 => ClipFrame::Loop(self.f32()?),
            1 => ClipFrame::Hold(self.u8()?),
            2 => ClipFrame::HoldRange { first: self.u8()?, last: self.u8()?, elapsed: self.f32()? },
            3 => ClipFrame::HoldFull { elapsed: self.f32()? },
            4 => ClipFrame::OnceFull { elapsed: self.f32()? },
            5 => ClipFrame::OnceFullDoubled { elapsed: self.f32()? },
            6 => ClipFrame::OnceFullSlowed { elapsed: self.f32()?, doubled_until: self.f32()? },
            _ => return Err("invalid terminal animation kind".into()),
        };
        Ok(Some(SceneAnimation { clip, frame, rate, once }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::scene::FadeOp;

    fn terminal() -> Cast {
        let mut cast = Cast::new([1, 2, 3]);
        let actor = cast.actors.get_mut(&1).unwrap();
        actor.pos = Some([-0.0, 537.0, -53.0]);
        actor.triangle = Some(15);
        actor.render_offset = [590, -38, -1316];
        actor.walked_frames = f32::from_bits(0x4329_ffe8);
        // Actor 2 is placed but hidden; actor 3 is unplaced. Both must survive capture.
        let hidden = cast.actors.get_mut(&2).unwrap();
        hidden.pos = Some([10.0, 20.0, -53.0]);
        hidden.visible = false;
        cast.apply_loop(1, 0, "aaaa/aafe", 3.25, false);
        cast.apply_hold_full(2, "bbba/bbcb", 9.75, true);
        cast.camera = CameraState::Scroll { x: 18, y: 2, frames: 0 };
        cast.camera_frames = f32::from_bits(0x4329_ffe8);
        cast.counts.camera_ops = 1;
        cast.background_ops.push(BackgroundOp::On { group: 3, frame: 1 });
        cast.shake = ShakeState::with_rng_indices([123, 231]);
        assert!(cast.shake.apply(2, 0, 0, 5, 2));
        for frame in [0.3, 1.7, 0.519, 2.0, 0.125] { cast.advance_effects(frame); }
        cast
    }

    #[test]
    fn actual_fractional_terminal_state_roundtrips_and_continues_identically() {
        let mut live = terminal();
        let checkpoint = live.checkpoint().unwrap();
        let decoded = CastCheckpoint::decode(&checkpoint.encode()).unwrap();
        assert_eq!(decoded, checkpoint);
        assert!(decoded.cast.background_ops.is_empty());
        let mut replay = terminal();
        replay.camera_frames = 170.0;
        replay.actors.get_mut(&1).unwrap().walked_frames = 170.0;
        replay.actors.get_mut(&1).unwrap().animation.as_mut().unwrap().frame = ClipFrame::Loop(3.0);
        replay.advance_effects(50.0); // the transcript cannot supply the actual dialogue dwell
        let mut restored = decoded.restore_against(&replay).unwrap();
        assert_eq!(restored.background_ops(), replay.background_ops());
        assert_eq!(restored.checkpoint().unwrap(), checkpoint);
        for frames in [0.1, 0.2, 0.7, 1.0, 17.519, 0.125, 512.0] {
            live.advance_effects(frames);
            restored.advance_effects(frames);
            assert_eq!(live.checkpoint().unwrap(), restored.checkpoint().unwrap());
        }
        assert_eq!(restored.actors.len(), 3);
        assert!(!restored.actors[&2].shown());
        assert_eq!(restored.actors[&3].pos, None);
        assert_eq!(restored.actors[&1].pos.unwrap()[0].to_bits(), (-0.0f32).to_bits());
    }

    #[test]
    fn replay_must_match_authored_shape_and_cannot_accept_injected_positions_or_clips() {
        let live = terminal();
        let saved = live.checkpoint().unwrap();
        let mut changed = live.clone();
        changed.actors.get_mut(&1).unwrap().pos.as_mut().unwrap()[0] = 10_000.0;
        assert!(saved.restore_against(&changed).is_err());
        let mut changed = live.clone();
        changed.actors.get_mut(&1).unwrap().animation.as_mut().unwrap().clip = "another/clip".into();
        assert!(saved.restore_against(&changed).is_err());
        let mut changed = live.clone();
        changed.actors.get_mut(&2).unwrap().visible = true;
        assert!(saved.restore_against(&changed).is_err());
        let mut changed = live.clone();
        changed.camera = CameraState::Default;
        assert!(saved.restore_against(&changed).is_err());
        let mut changed = live.clone();
        changed.counts.anims_skipped += 1;
        assert!(saved.restore_against(&changed).is_err());
        let mut changed = live.clone();
        changed.actors.get_mut(&1).unwrap().default_animation.as_mut().unwrap().slot = 2;
        assert!(saved.restore_against(&changed).is_err());
        let mut forged = live.clone();
        assert!(forged.shake.apply(2, 0, 0, 6, 2));
        assert!(forged.checkpoint().unwrap().restore_against(&live).is_err(), "authored shake amplitude cannot change");
        let mut forged = live.clone();
        assert!(forged.fade.apply(FadeOp::Nfade { kind: 11, rgb: [120, 60, 30], frames: 30 }));
        let saved = forged.checkpoint().unwrap();
        assert_eq!(CastCheckpoint::decode(&saved.encode()).unwrap(), saved, "the codec retains all fade state");
        assert!(saved.restore_against(&live).is_err(), "train cannot acquire a different fade instruction");
    }

    #[test]
    fn unsettled_motion_and_invalid_phase_are_not_rounded_or_settled_on_capture() {
        let mut cast = terminal();
        cast.actors.get_mut(&1).unwrap().turn = Some(Turn { from: 0.0, to: 1.0, frames: 10.0, elapsed: 4.0 });
        assert!(cast.checkpoint().is_err());
        assert!(terminal().checkpoint().unwrap().restore_against(&cast).is_err());
        let mut cast = terminal();
        cast.actors.get_mut(&1).unwrap().offset_motion = Some((crate::script::scene::OffsetMotion::new([0; 3], [1; 3], 10).unwrap(), 4.0));
        assert!(cast.checkpoint().is_err());
        for bad in [f32::NAN, f32::INFINITY, -0.25] {
            let mut cast = terminal();
            cast.actors.get_mut(&1).unwrap().animation.as_mut().unwrap().frame = ClipFrame::Loop(bad);
            assert!(cast.checkpoint().is_err());
        }
    }

    #[test]
    fn every_animation_descriptor_has_an_exact_encoding() {
        for frame in [ClipFrame::Loop(-0.0), ClipFrame::Hold(7), ClipFrame::HoldRange { first: 2, last: 9, elapsed: 4.125 },
            ClipFrame::HoldFull { elapsed: 17.3 }, ClipFrame::OnceFull { elapsed: 18.3 },
            ClipFrame::OnceFullDoubled { elapsed: 19.3 }, ClipFrame::OnceFullSlowed { elapsed: 20.3, doubled_until: 8.2 }] {
            let mut cast = terminal();
            cast.actors.get_mut(&1).unwrap().animation.as_mut().unwrap().frame = frame;
            let saved = cast.checkpoint().unwrap();
            assert_eq!(CastCheckpoint::decode(&saved.encode()).unwrap(), saved);
        }
        let mut positive = terminal();
        let mut negative = positive.clone();
        positive.camera_frames = 0.0;
        negative.camera_frames = -0.0;
        assert_ne!(positive.checkpoint().unwrap(), negative.checkpoint().unwrap(), "equality compares float bits");
    }

    #[test]
    fn malformed_truncated_trailing_and_oversized_encodings_are_refused() {
        let bytes = terminal().checkpoint().unwrap().encode();
        for end in 0..bytes.len() { assert!(CastCheckpoint::decode(&bytes[..end]).is_err(), "prefix {end}"); }
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(CastCheckpoint::decode(&bad).is_err());
        let mut bad = bytes.clone();
        bad[4] = 2;
        assert!(CastCheckpoint::decode(&bad).is_err());
        let mut bad = bytes.clone();
        bad[6] = 10;
        assert!(CastCheckpoint::decode(&bad).is_err());
        let mut bad = bytes.clone();
        bad[8] = 2; // first actor's position-present boolean
        assert!(CastCheckpoint::decode(&bad).is_err());
        assert!(CastCheckpoint::decode(&vec![0; MAX_CAST_CHECKPOINT_BYTES + 1]).is_err());
    }

    #[test]
    fn the_maximum_nine_actor_encoding_is_exactly_7630_bytes() {
        let mut cast = Cast::new(0..9);
        for actor in cast.actors.values_mut() {
            actor.pos = Some([0.0; 3]);
            actor.triangle = Some(0);
            let animation = SceneAnimation { clip: "a".repeat(MAX_CLIP_BYTES), frame: ClipFrame::OnceFullSlowed { elapsed: 1.0, doubled_until: 0.5 }, rate: 1.0, once: true };
            actor.animation = Some(animation.clone());
            actor.movement_animation = Some(animation);
            actor.default_animation = Some(DefaultAnimation { slot: 0, clip: "b".repeat(MAX_CLIP_BYTES) });
        }
        cast.camera = CameraState::Scroll { x: 0, y: 0, frames: 0 };
        let saved = cast.checkpoint().unwrap();
        assert_eq!(saved.encode().len(), 7_630);
        assert_eq!(saved.encode().len(), MAX_CAST_CHECKPOINT_BYTES);
        assert_eq!(CastCheckpoint::decode(&saved.encode()).unwrap(), saved);
        cast.actors.get_mut(&0).unwrap().animation.as_mut().unwrap().clip.push('x');
        assert!(cast.checkpoint().is_err());
        assert!(Cast::new(0..10).checkpoint().is_err());
    }
}
