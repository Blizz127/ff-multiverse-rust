//! The ladders and jumps of a field: the lines whose scripts put the player's character somewhere else in the SAME
//! field (a jump over a gap, a climb up or down a ladder).
//!
//! # What the data says
//!
//! A `LINE` entity watches a line (its init part says where, and whether it starts enabled) and has up to six trigger
//! scripts. The engine runs one when the character touches the line (the `[OK]` slot when the player presses the
//! button there, the others as the character walks on, or stands on it: those poll the key themselves with `IFKEYON`).
//! What these scripts do in the first reactor mission is ask the player's own entity to run one of its scripts
//! (`REQ`/`REQEW entity script`), and those scripts move the character: `JUMP` (a hop to a place on a triangle), `LADER`
//! (a climb to a place on a triangle), `MOVE` (a walk), and sometimes read the place back (`AXYZI`, `GETAI`) to decide
//! on a follow-up jump from where the climb ended.
//!
//! # How a link is read
//!
//! Every enabled line entity's trigger scripts (slots 1 to 6, slots that share an entry point being one script) are
//! WALKED with the dialogue walker ([`crate::script::dialogue::Walk`], following movements): once with no key pressed,
//! which is what walking up to the line does, and, if that moved nobody, once with OK pressed. A script that ends
//! normally with the player's entity moved is a link: **walk-on** when it moved the player with no key, **needs OK** when
//! it only did with OK pressed. Its destination is where the walk left the player: the place, and the triangle the last
//! `JUMP`/`LADER` named (the hint that picks the right floor where triangles overlap). Nothing is timed or animated:
//! a movement is its end.
//!
//! A walk reads nothing of the story: the variables a ladder compares are the ones the walk itself wrote (the place it
//! read back) and the constants the init parts of the field's entities set (the triangle a ladder expects to end on),
//! which seed the walk. A script that reads any other variable is **dropped, not guessed**, and so is one whose
//! read-back the walk could not fill, one that does not end normally (a map jump, a battle, a wait for ever), one whose
//! destination names no triangle of the walkmesh or lies outside the triangle it names, and one that moves
//! nobody. [`scan_links`] reports what it dropped and why.
//!
//! **Only in the [`crate::REVIEWED_FIELDS`]**: like script exits and story points, a field's links are offered only if
//! the field was reviewed (a link teleports a player: an unreviewed script must not do it to a live account).
//! `AreaRuntime::story_withheld` names the others.
//!
//! # Untrusted data
//!
//! Like the rest of the crate: no indexing, no unchecked arithmetic, no panics on script bytes; every walk is bounded by
//! [`LINK_LIMITS`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use fmv_adapter_api::Link;
use fmv_core::Pos;

use crate::field::Walkmesh;
use crate::script::dialogue::{Beat, End, Limits, StoryVars, Walk, KEY_OK};
use crate::script::{triangle_contains_xy, triangle_height_at, FieldScript, INIT_SCRIPT, LINE_SLOTS};
use crate::story::scan::{init_bytes, init_part, line_of};

/// What one link walk may do: a ladder is a few dozen instructions, so this is far above need and ends a wait that
/// never ends quickly.
pub const LINK_LIMITS: Limits = Limits { steps: 3000, beats: 8, depth: 4 };

/// What the character does along a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// A climb (`LADER`) is part of it.
    Ladder,
    /// A jump (`JUMP`) and no climb.
    Jump,
    /// Only walking (`MOVE`) or placing (`XYZI`).
    Move,
}

impl LinkKind {
    /// The word a log uses for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ladder => "ladder",
            Self::Jump => "jump",
            Self::Move => "move",
        }
    }
}

/// One link as the field's scripts define it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkDef {
    /// The line entity and the trigger script that sets it off.
    pub entity: usize,
    pub slot: usize,
    /// The line, in the field's coordinates (z the line's height).
    pub line: (Pos, Pos),
    /// Where the player ends up: x and y as the script says, z the height if the script says it (0 otherwise: the area
    /// puts the player on its own surface).
    pub to: Pos,
    /// The walkmesh triangle the script names for the destination.
    pub triangle: u32,
    /// The line waits for the OK button.
    pub needs_ok: bool,
    pub kind: LinkKind,
}

impl LinkDef {
    /// The link as the adapter API offers it.
    pub fn link(&self) -> Link {
        Link { line: self.line, to: self.to, to_hint: Some(self.triangle), needs_ok: self.needs_ok, label: self.kind.label().to_string() }
    }

    /// One line for the log of what a field withholds: numbers and words of our own, never text of the game.
    pub fn describe(&self) -> String {
        format!(
            "{} of line entity {} (script {}), {} to triangle {}",
            self.kind.label(),
            self.entity,
            self.slot,
            if self.needs_ok { "on OK" } else { "on walking up" },
            self.triangle
        )
    }
}

/// What [`scan_links`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkScan {
    /// The links, in the order of the line entities and their scripts.
    pub found: Vec<LinkDef>,
    /// The scripts that moved the player and were dropped, with the line entity, the slot and the reason.
    pub dropped: Vec<(usize, usize, &'static str)>,
}

/// What a link walk reads: the constants the init parts set (seeding it), the walkmesh's heights, and nothing else.
struct LinkEnv<'a> {
    seeds: &'a BTreeMap<(u8, u8), u8>,
    mesh: &'a Walkmesh,
    /// Variables the walk asked for that nothing seeded (it reads the story, or state nobody here knows).
    unseeded: RefCell<BTreeSet<(u8, u8)>>,
}

impl StoryVars for LinkEnv<'_> {
    fn byte(&self, bank: u8, address: u8) -> u8 {
        match self.seeds.get(&(bank, address)) {
            Some(&value) => value,
            None => {
                self.unseeded.borrow_mut().insert((bank, address));
                0
            }
        }
    }

    fn party_member(&self, slot: u8) -> Option<u8> {
        // (the first mission's party; no link script asks)
        match slot {
            0 => Some(0),
            1 => Some(1),
            _ => None,
        }
    }

    fn height(&self, x: i32, y: i32, triangle: Option<u16>) -> Option<i32> {
        let tri = self.mesh.triangles.get(usize::from(triangle?))?;
        let z = triangle_height_at(tri, x, y)?;
        z.is_finite().then(|| z.round().clamp(-32768.0, 32767.0) as i32)
    }
}

/// What walking one trigger script came to.
struct Attempt {
    /// The walk, when the script ended normally and moved a player (`leader` is the player entity it moved first).
    moved: Option<(Walk, usize)>,
    /// The walk asked for a variable nothing seeded.
    unseeded: bool,
}

/// Walk script `slot` of `entity` with `pressed` held. A script that does not end normally (a map jump, a battle, a wait
/// for ever) moves nobody as far as a link is concerned.
fn attempt(script: &FieldScript<'_>, entity: usize, slot: usize, pressed: u32, players: &BTreeSet<usize>, env: &LinkEnv<'_>) -> Attempt {
    let mut walk = Walk::script(entity, slot, 1, LINK_LIMITS).pressing(pressed).tracking_moves();
    // (every window the script opens is one more beat; the limits end it long before this does)
    let mut finished = false;
    for _ in 0..=LINK_LIMITS.beats.saturating_add(2) {
        match walk.resume(script, env, None) {
            Beat::End(end) => {
                finished = end == End::Finished;
                break;
            }
            Beat::Message(_) => {}
        }
    }
    let unseeded = !env.unseeded.borrow().is_empty();
    let leader = walk.moves().iter().map(|(e, _)| *e).find(|e| players.contains(e));
    Attempt { moved: leader.filter(|_| finished).map(|leader| (walk, leader)), unseeded }
}

/// The links of a field: see the module docs.
pub fn scan_links(script: &FieldScript<'_>, mesh: &Walkmesh) -> LinkScan {
    let players: BTreeSet<usize> = script.player_entities().into_iter().map(|(entity, _)| entity).collect();
    // Every init part's constants seed the walks. A variable two entities set differently is nobody's constant.
    let mut seeds: BTreeMap<(u8, u8), u8> = BTreeMap::new();
    let mut clash: BTreeSet<(u8, u8)> = BTreeSet::new();
    let mut lines = Vec::new();
    for entity in 0..script.entity_count() {
        let Ok(listing) = script.disassemble_partial(entity, INIT_SCRIPT) else { continue };
        let init = init_part(&listing.instructions);
        for (key, value) in init_bytes(init) {
            if seeds.insert(key, value).is_some_and(|before| before != value) {
                clash.insert(key);
            }
        }
        if let Some(line) = line_of(init).filter(|l| l.enabled != Some(false)) {
            lines.push((entity, line));
        }
    }
    for key in &clash {
        seeds.remove(key);
    }

    let mut scan = LinkScan::default();
    for (entity, line) in lines {
        let mut seen = BTreeSet::new();
        for slot in LINE_SLOTS.map(usize::from) {
            let Some(span) = script.span(entity, slot) else { continue };
            if !seen.insert(span.start) {
                continue;
            }
            // Walking up to the line first; only if that moves nobody, the press of OK.
            let env = LinkEnv { seeds: &seeds, mesh, unseeded: RefCell::new(BTreeSet::new()) };
            let walked = attempt(script, entity, slot, 0, &players, &env);
            let (chosen, needs_ok, unseeded) = if walked.moved.is_some() {
                (walked.moved, false, walked.unseeded)
            } else {
                let env = LinkEnv { seeds: &seeds, mesh, unseeded: RefCell::new(BTreeSet::new()) };
                let pressed = attempt(script, entity, slot, KEY_OK, &players, &env);
                (pressed.moved, true, pressed.unseeded || walked.unseeded)
            };
            let mut reject = |why: &'static str| scan.dropped.push((entity, slot, why));
            let Some((walk, leader)) = chosen else {
                // It moves nobody as things are, and asks for something nothing here knows: it may move somebody in
                // another state of the story, which is not for a link.
                if unseeded {
                    reject("moves nobody now and reads a variable nothing here knows");
                }
                continue;
            };
            let Some(spot) = walk.spot(leader) else { continue };
            if unseeded {
                reject("reads a variable nothing here knows");
                continue;
            }
            if walk.unresolved() {
                reject("reads back a place the walk cannot know");
                continue;
            }
            let Some(triangle) = spot.triangle.and_then(|t| mesh.triangles.get(usize::from(t)).map(|tri| (t, tri))) else {
                reject("ends where no triangle of the walkmesh is named");
                continue;
            };
            if !triangle_contains_xy(triangle.1, spot.x, spot.y) {
                reject("ends outside the triangle it names");
                continue;
            }
            let by_leader = walk.moves().iter().filter(|(e, _)| *e == leader);
            let (mut ladder, mut jump) = (false, false);
            for (_, s) in by_leader {
                ladder |= s.by == "LADER";
                jump |= s.by == "JUMP";
            }
            let kind = if ladder {
                LinkKind::Ladder
            } else if jump {
                LinkKind::Jump
            } else {
                LinkKind::Move
            };
            let z = spot.z.map_or(0.0, |z| z as f32);
            scan.found.push(LinkDef {
                entity,
                slot,
                line: (line.a, line.b),
                to: Pos::new(spot.x as f32, spot.y as f32, z),
                triangle: u32::from(triangle.0),
                needs_ok,
                kind,
            });
        }
    }
    scan
}

#[cfg(test)]
mod tests;
