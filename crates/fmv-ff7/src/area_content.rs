//! What an FF7 field's own data says about its surroundings, in the adapter-api's terms: the
//! gateway [`Exit`]s, the non-player characters ([`NpcSpawn`]) and the conversion of the script's
//! `DIR` byte to a protocol heading. [`crate::Ff7FieldArea`] computes all of it once, when a
//! field is loaded, and hands it out through `AreaRuntime::{exits, npcs}`.
//!
//! Nothing here evaluates a script: only literal operands are read, and a placement that depends
//! on game variables (a conditional one, or one whose operands name variables) is left out. The
//! script VM and the story variables are what a fuller version would need.
//!
//! # Exits: line gateways
//!
//! The twelve gateway slots of the trigger section ([`crate::field::Triggers::gateways`]) are
//! joined with the archive's `maplist` entry ([`crate::script::resolve_gateways`]): a used slot
//! becomes an [`Exit`] whose `line` is the slot's exit line in this field's coordinates, whose
//! `arrival` is the slot's destination `(x, y)` in the target's coordinates (`z` is the
//! placeholder `0.0`, see `Exit::arrival`) and whose `arrival_hint` is the index of the target
//! walkmesh triangle that holds that point (see [`crate::Ff7FieldArea`]'s `place_arrival`).
//! `arrival_facing` is `None`: a gateway carries no heading (the arrival field's own script
//! turns the character).
//!
//! The `target` is spelled the way the archive spells the entry (which is what `list_areas`
//! offers), so a server can hand it straight to `load_area`. A gateway is dropped, not reported,
//! when its id is past the end of the maplist, when the name is not an entry of the archive, when
//! the entry is not offered as an area (its name fails the adapter's key rule, or it is the name
//! list itself). The adapter applies the same per-name rules as its index; what it cannot mirror
//! without building the whole index (which reads every entry) is the index's work budget and
//! entries that failed to read.
//!
//! **Not here:** the exits that scripts decide (the `MAPJUMP`s of a line's walk-on trigger script, with the
//! conditions on the story they sit under) are built by `crate::exit_gate` from
//! [`crate::script::FieldScript::script_exits`] and follow the gateways in `exits()`; this module is the data
//! gateways only. 1 036 of the owned data's gateways are line gateways and 1 801 literal `MAPJUMP`s exist besides.
//!
//! **Measured on the owned FF7 Steam Edition (en), 2026-11-08** (`tests/area_real.rs`): of the 1 036
//! used gateways all 1 036 ids are in the maplist, 978 name an area the archive offers (the other 58
//! name no archive entry) and become exits, spread over 454 of the 702 fields (248 have none; one
//! has ten). All 978 arrival points lie inside the target triangle their hint names, every target
//! loads, and for 22 exits (layered meshes) the hint changes the arrival height by more than a unit
//! from what the unhinted rule would give. `place_arrival` returns the point without running it
//! through the movement rules, and `validate_move` accepts each of the 978 placed arrivals as it is
//! (at most the 0.05 wall nudge in x and y, under one unit in z).
//!
//! # NPCs: unconditional placements only
//!
//! An entity is reported when
//!
//! * its script 0 runs `CHAR` (it is a field model) and not `PC` (it does not stand for a
//!   playable character: those are the players), and the model-loader section has the record
//!   `CHAR` stands for ([`crate::script::EntityInit::model_ref`]);
//! * its **init part** (script 0 up to its first `RET`, run once when the field loads) places it
//!   unconditionally with literal `x` and `y` (`XYZI`, `XYI` or `XYZ` outside any region a jump
//!   skips); the last such placement counts. `z` is the placement's literal `z` if it has one; an
//!   `XYI`, or a placement whose `z` names a variable, takes the walkmesh height at `(x, y)`
//!   instead (on the triangle the placement names when that triangle holds the point, otherwise
//!   on [`crate::Ff7FieldArea::height_at`]'s surface), and an entity whose point is on no
//!   triangle is skipped;
//! * nothing hides it: the last unconditional `VISI` of its script 0 is not a literal `0`, whether
//!   that instruction is in the init part or in the main part (which starts running in the first
//!   frame; 16 owned entities that the init part places are hidden by an unconditional `VISI 0`
//!   of their main part, for example `corneo` of `datiao_6`). A character that starts hidden is shown later by a
//!   script (a cutscene, a story flag), which is story-dependent; one that is only *sometimes*
//!   shown or hidden by a conditional `VISI` is judged by its unconditional ones.
//!
//! Placements in the main part (after the first `RET`) and conditional ones are left out
//! (`NpcSpawn::unconditional` is therefore `true` for every NPC the data places; the flag stays so a later
//! version can report story-dependent placements).
//!
//! **No `PC` entity is ever an NPC.** The party's canon members are in the BATTLE party, not in the field: the
//! story's canon party (`AreaRuntime::story_party`) fights beside the players as server-controlled companions,
//! and the field shows only the leader the player controls (FF7's own field shows only its party leader too). An
//! earlier slice rule that drew the canon Barret as an NPC of `md1stin` and `md1_1` was removed for that reason.
//! What an NPC looks like comes from the model-loader
//! record: the skeleton stem, the first animation (by the loader's own ordering convention the
//! character's standing clip) and the record's name (cut to [`MAX_NPC_LABEL_CHARS`] characters).
//! The facing is [`field_direction_to_facing`] of the last literal `DIR`, byte 0 without one. At most
//! [`MAX_NPCS_PER_AREA`] are reported. A damaged script or model-loader section, or one larger than
//! [`MAX_NPC_SECTION_BYTES`], yields no NPCs, never an error.
//!
//! **Measured on the owned copy:** 1 134 NPCs in 385 of the 702 fields (14 in the busiest); all of
//! them stand at a literal `XYZI` height (the walkmesh height was never needed) and two of the
//! points lie on no walkmesh triangle (`pinball` of `mds7pb_2` and `fm1` of `mtcrl_7`; they are
//! reported at their literal positions, so an NPC's `pos` is not always on the walkable surface). Of the
//! entities that are not reported, 3 042 are players, 5 080 are no field model, 606 + 48 are hidden
//! (init part + main part), 343 are placed only conditionally, 120 only by the main part, 144 have
//! no placement and 17 have operands that name variables. Of the reported NPCs 352 have no `DIR`
//! (so face byte 0), 8 have only a conditional one and 774 an unconditional literal one.
//! **Entity names are labels, not identifiers:** 54 of the 1 134 NPCs have an empty name and in
//! 13 fields two NPCs share one (seven `blin66_1` NPCs have the same empty name).
//!
//! # The `DIR` byte and the protocol heading
//!
//! See [`field_direction_to_facing`] for the one conversion and for the evidence behind it.

use crate::field::{section, FieldFile, LoaderAnimation, ModelLoader, Walkmesh};
use crate::script::{
    parse_model_list, resolve_gateways, triangle_contains_xy, triangle_height_at, Arg, EntityInit, FieldScript, Maplist, Phase,
    Placement,
};
use crate::util::entry_stem;
use fmv_adapter_api::{Exit, ExitKind, NpcAppearance, NpcSpawn};
use fmv_core::Pos;

/// Most NPCs one area reports. (A script section has at most 255 entities, so real fields cannot
/// reach it; it only bounds what a hostile section could cost.)
pub const MAX_NPCS_PER_AREA: usize = 256;

/// Longest `NpcAppearance::label` kept, in characters (a model-loader name can be 65 535 bytes
/// long; real ones are about 25).
pub const MAX_NPC_LABEL_CHARS: usize = 64;

/// Largest script section or model-loader section (bytes) the NPC analysis reads; a field with a
/// bigger one reports no NPCs. The work is linear in the section, once per entity at worst (entities
/// may share one script), so this bounds what a hostile section costs at load. The real ones are far
/// smaller: at most 60 544 bytes for a script section (`frcyo`) and 3 027 for a model-loader
/// section in the owned copy (`tests/area_real.rs`, `script_and_model_loader_sections_are_small`).
pub const MAX_NPC_SECTION_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------------------------------
// DIR byte -> heading
// ---------------------------------------------------------------------------------------

/// Units (1/256 turn) of the script's direction byte between byte 0 and the heading 0 (facing the
/// field's +x axis): the byte 64 faces +x. See [`field_direction_to_facing`].
const DIR_BYTE_OF_HEADING_ZERO: i32 = 64;

/// Convert a script direction byte (the operand of `DIR`, 256 to a full turn) to the protocol's
/// heading: radians in `(-pi, pi]`, counter-clockwise from the field's +x axis seen from above
/// (`atan2(dy, dx)` of the way the character faces), which is what `model_to_field` and
/// `EntitySnapshot::facing` use. This is the ONE place the conversion lives:
///
/// ```text
/// facing = (direction - 64) / 256 * tau        wrapped into (-pi, pi]
/// ```
///
/// so byte 64 faces +x, 128 faces +y, 192 faces -x and 0 faces -y, and the byte counts
/// counter-clockwise seen from above (the way the heading angle does).
///
/// **The map is inferred from the data only; no documentation was relied on.** The one community
/// page that was checked (FFRTT, opcode B3 `DIR`,
/// <https://wiki.ffrtt.ru/index.php/FF7/Field/Script/Opcodes/B3_DIR>, 2026-11-08) says just that the
/// operand is "in the standard direction format for the game": no unit, no zero direction, no
/// rotation sense, and nothing about the camera. The map is the linear one that carries the
/// field's `control_direction` (the trigger section's byte for the direction of "up" in the
/// movement controls, same unit) onto the world heading of "up the screen" through the field
/// camera, and the one under which characters that arrive by `MAPJUMP` face into the field. The
/// two checks are repeated by the `#[ignore]`d real-data tests of `tests/area_real.rs`
/// (`dir_convention_*`). On the owned Steam copy (702 fields, 2026-11-08):
///
/// * **The camera check.** The heading of the camera's forward axis on the ground against
///   `control_direction`, over the 688 fields whose camera is not near-vertical. Counting
///   counter-clockwise (+1) fits far better than clockwise (-1) (resultant length 0.94 against
///   0.60), and the best free offset, -86.9 degrees (-61.8/256 turn), is 3 degrees from the quarter
///   turn used here. With this map the residual has a median of 4.6 degrees, 75 % of the fields lie
///   within 20 degrees and 96 % within 45; `md1stin` (`control_direction` 124) is 1.3 degrees off.
///   **The per-field scatter is large**: 408 of the 699 fields have `control_direction == 128`,
///   which looks like a default (more than half of their cameras look within 5 degrees of +y, so
///   it fits them with a median of 1.5 degrees, but a few fall 50 to 145 degrees off,
///   `sininb34` and `sininb36` being the worst);
///   for the 289 fields that set another value the sign still wins clearly (resultant 0.96 against
///   0.23) and the free offset is -87.8 degrees, but the residual of this map has a median of 6.5
///   degrees, 24 % of the fields lie farther than 20 degrees and the worst is 53. So the byte is
///   only approximately the camera's heading (designers set it by hand, and a perspective camera has
///   no single "up"), which limits how well this check can pin the offset to about the 3 degrees seen.
///   The other measure, the direction that appears as "up the screen" at the middle of the walkmesh
///   (the local slope of the camera projection, inverted the way the client's
///   `FieldView::screen_to_world_dir` does), gives the same sign and almost the same offset (-88.0
///   degrees, resultant 0.85) but a looser fit: median 6.2 degrees, 73 % of the 699 fields within
///   20 degrees, 91 % within 45, and 38 fields more than 90 degrees off (the twelve worst, among
///   them `coloss`, `elevtr1` and `gaia_32`, 177 to 180 degrees). For 27 of those 38 the forward
///   axis does fit (within 20 degrees), for the other 11 neither measure does; why the screen-up
///   measure flips was not investigated. The forward axis is the steadier of the two, so the
///   numbers above lead with it.
/// * **The `MAPJUMP` check.** Over the 1 179 literal `MAPJUMP`s that land inside the triangle they
///   name (those in script 0's conditional part left out), the mean cosine between the facing this
///   map gives and the direction from the arrival toward the middle of the target's walkmesh is
///   0.406 (standard error 0.021; chance is 0, a perfect map 1), and 73.7 % of the arrivals face
///   within 90 degrees of inward. The best of all 512 linear maps scores 0.407 and sits 3/256 of a
///   turn from this one; the clockwise maps (sampled at eight offsets) score between -0.02 and
///   +0.02, no better than chance.
///
/// Both checks agree on the sign and put the offset within 3/256 of a turn of the quarter turn on
/// either side, which is as far as this data resolves it. Still **UNVERIFIED** against the game
/// itself (the model's own `-z` front and any per-model rest angle are not part of the evidence).
///
/// An entity without a `DIR` (or one whose operand names a variable) has the engine's initial
/// direction, byte 0, which this maps to facing -y (down the screen on the many fields whose
/// `control_direction` is 128).
pub fn field_direction_to_facing(direction: u8) -> f32 {
    // whole units of 1/256 turn: exact at the quarter turns, no float noise to wrap
    let units = (i32::from(direction) - DIR_BYTE_OF_HEADING_ZERO).rem_euclid(256);
    let units = if units > 128 { units - 256 } else { units }; // (-128, 128]: 128 is +pi
    (f64::from(units) / 256.0 * std::f64::consts::TAU) as f32
}

/// The script's `DIR` byte nearest a protocol heading (radians, counter-clockwise from +x): the inverse of
/// [`field_direction_to_facing`], exact at every whole 1/256 turn. A heading that is not finite gives the byte of heading 0.
pub fn facing_to_field_direction(facing: f32) -> u8 {
    if !facing.is_finite() {
        return u8::try_from(DIR_BYTE_OF_HEADING_ZERO).unwrap_or(0);
    }
    let units = (f64::from(facing) / std::f64::consts::TAU * 256.0).round() as i64;
    u8::try_from((units + i64::from(DIR_BYTE_OF_HEADING_ZERO)).rem_euclid(256)).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// Exits
// ---------------------------------------------------------------------------------------

fn gateway_pos(p: [i16; 3]) -> Pos {
    Pos::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2]))
}

/// The gateway exits of `field`. `listed_key` maps a maplist name to the key under which the
/// archive offers that area (`None`: it does not), see the module docs. A field whose triggers
/// section does not parse has no exits.
pub(crate) fn gateway_exits(field: &FieldFile, maplist: &Maplist, listed_key: impl Fn(&str) -> Option<String>) -> Vec<Exit> {
    let Ok(triggers) = field.triggers() else { return Vec::new() };
    resolve_gateways(&triggers, maplist)
        .into_iter()
        .filter_map(|g| {
            let target = listed_key(g.target_field.as_deref()?)?;
            Some(Exit {
                line: (gateway_pos(g.exit_a), gateway_pos(g.exit_b)),
                target,
                arrival: Pos::new(f32::from(g.destination_x), f32::from(g.destination_y), 0.0),
                arrival_facing: None,
                arrival_hint: g.destination_triangle.and_then(|t| u32::try_from(t).ok()),
                kind: ExitKind::Gateway,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// NPCs
// ---------------------------------------------------------------------------------------

/// The NPCs of `field` (see the module docs for the rules). `walkmesh` is the field's own, and
/// `height_at(x, y)` the surface height at a point, for placements without a literal `z`.
pub(crate) fn npc_spawns(field: &FieldFile, walkmesh: &Walkmesh, height_at: impl Fn(f32, f32) -> Option<f32>) -> Vec<NpcSpawn> {
    npc_spawns_capped(field, walkmesh, height_at, MAX_NPCS_PER_AREA)
}

/// [`npc_spawns`] with the cap as a parameter (so that a test can reach it).
pub(crate) fn npc_spawns_capped(
    field: &FieldFile,
    walkmesh: &Walkmesh,
    height_at: impl Fn(f32, f32) -> Option<f32>,
    cap: usize,
) -> Vec<NpcSpawn> {
    let small = |index: usize| field.section(index).is_ok_and(|s| s.len() <= MAX_NPC_SECTION_BYTES);
    if !(small(section::SCRIPT) && small(section::MODEL_LOADER)) {
        return Vec::new();
    }
    let Ok(script) = FieldScript::from_field(field) else { return Vec::new() };
    let Some(models) = field.section(section::MODEL_LOADER).ok().and_then(|s| parse_model_list(s).ok()) else {
        return Vec::new();
    };
    let loader = field.model_loader().ok();
    script
        .entity_inits(Some(&models))
        .iter()
        .filter_map(|init| npc_of(init, loader.as_ref(), walkmesh, &height_at))
        .take(cap)
        .collect()
}

/// Whether the entity's own script hides it for good: its last unconditional `VISI` is a literal 0.
fn hidden_by_script(init: &EntityInit) -> bool {
    init.visible.is_some_and(|v| !v.conditional && v.value == Arg::Literal(0))
}

/// The last placement of the init part that always runs.
fn init_placement(init: &EntityInit) -> Option<&Placement> {
    init.placements.iter().rev().find(|p| p.phase == Phase::Init && !p.conditional)
}

fn npc_of(
    init: &EntityInit,
    loader: Option<&ModelLoader>,
    walkmesh: &Walkmesh,
    height_at: &impl Fn(f32, f32) -> Option<f32>,
) -> Option<NpcSpawn> {
    if !init.is_character() || init.is_player() || hidden_by_script(init) {
        return None;
    }
    let placement = init_placement(init)?;
    let model = init.model_ref.as_ref()?;
    let (x, y) = placement.xy()?;
    let z = match placement.z.literal() {
        Some(z) => f32::from(z),
        None => surface_height(walkmesh, placement.triangle, x, y).or_else(|| height_at(f32::from(x), f32::from(y)))?,
    };
    let pos = Pos::new(f32::from(x), f32::from(y), z);
    if !pos.is_finite() {
        return None;
    }
    let stem = entry_stem(&model.hrc);
    if stem.is_empty() {
        return None;
    }
    let idle_animation = loader
        .zip(init.model_index)
        .and_then(|(l, i)| l.models.get(i))
        .and_then(|record| record.animations.first())
        .map(LoaderAnimation::stem)
        .filter(|s| !s.is_empty());
    let direction = init.direction.and_then(|d| d.value.literal()).unwrap_or(0);
    Some(NpcSpawn {
        entity_name: init.name.clone(),
        appearance: NpcAppearance { model: stem, idle_animation, label: model.name.chars().take(MAX_NPC_LABEL_CHARS).collect() },
        pos,
        facing: field_direction_to_facing(direction),
        unconditional: true,
    })
}

/// Height of the walkmesh triangle a placement names, at `(x, y)`, when that triangle holds the
/// point (the way to pick the right floor of a layered field). `None` when the placement names no
/// triangle (or a variable one), the triangle is not on the mesh, or it does not hold the point.
fn surface_height(walkmesh: &Walkmesh, triangle: Arg<u16>, x: i16, y: i16) -> Option<f32> {
    let tri = walkmesh.triangles.get(usize::from(triangle.literal()?))?;
    let (x, y) = (i32::from(x), i32::from(y));
    triangle_contains_xy(tri, x, y).then(|| triangle_height_at(tri, x, y)).flatten().map(|z| z as f32).filter(|z| z.is_finite())
}
