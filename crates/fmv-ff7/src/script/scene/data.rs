//! Scene DATA: what a story scene is, as ids, names and numbers only.
//!
//! A [`SceneDef`] never holds game text or bytes. It points into a field's own script (a field name, script
//! entities and slots) and says which actors play which entity; the script, the lines and the dialog table are
//! read from the owner's files when the scene runs ([`super::ScenePlay`]). Because a scene is data, the same one
//! can be played with another cast ([`SceneDef::with_cast`]): the script's entities stay the same, the models that
//! stand for them change.
//!
//! # The shape
//!
//! * **Trigger** ([`Trigger`]): the character whose talk starts the scene, the story moments it is offered at, and
//!   how near a player has to stand. The trigger entity is usually one of the game's own party members (a `PC`
//!   entity), which the server never announces as an NPC, so the server adds it to the area's cast while the scene is
//!   on offer (`fmv_protocol::SCENE_NPC_ID_BASE`).
//! * **Context** ([`Given`]): the field variables the scene assumes. A scene's script branches on variables the story
//!   model does not carry (a field's own progress flags): the scene says which values it was found with, and the
//!   story the player really has decides the rest (the moment).
//! * **Stage** ([`SceneDef::setup`]): scripts of the field that put the actors where they stand when the scene
//!   begins, run once and quietly (no windows, nothing kept but the places and the variables).
//! * **Root** ([`Step`]): what the field's own trigger does to start the scene, in its order.
//! * **Cast** ([`CastMember`]): which script entity is which actor, and a start place for one the stage scripts
//!   do not place.
//! * **Speakers**: a window's first line names who speaks (the game's own convention); this maps those names to
//!   cast entities.
//! * **Witness** ([`Spot`]s): where the player's own character stands (the first spot) and where party members
//!   that watch stand (the others).

use crate::story::StoryState;
use crate::script::FieldScript;
use std::ops::RangeInclusive;

/// A script of a field: its entity (index in the field's script) and slot (0..32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScriptRef {
    pub entity: u8,
    pub slot: u8,
}

impl ScriptRef {
    pub const fn new(entity: u8, slot: u8) -> Self {
        Self { entity, slot }
    }
}

/// One thing the field's own trigger does to start a scene, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `BITON`: turn bit `bit` of variable `bank`/`address` on (the field marks the scene as begun first).
    SetBit { bank: u8, address: u8, bit: u8 },
    /// Start a script and wait for it to end (`REQEW`).
    Await(ScriptRef),
    /// Start a script and go on (`REQSW`): the scene ends when it and everything it started has.
    Start(ScriptRef),
    /// Start the main part of an entity's script 0 (after its initial RET).
    StartMain(u8),
}

/// A field variable (one byte) the scene assumes when it is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Given {
    pub bank: u8,
    pub address: u8,
    pub value: u8,
}

/// Where somebody stands: walkmesh position, the triangle that holds it and the script's `DIR` byte (256 to a turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    pub x: i16,
    pub y: i16,
    pub triangle: u16,
    pub direction: u8,
}

/// How an actor looks. The models are the client's own (`char.lgp`); the scene only names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Look {
    /// One of the playable table's appearances (`ff7/cloud`, `ff7/tifa`: idle, walk and run clips).
    Appearance(String),
    /// A model by its `char.lgp` skeleton stem and standing clip stem (an NPC's way of being named).
    Model { hrc: String, idle: String },
}

/// One actor of the cast: the script entity it stands for and how it looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastMember {
    /// The script entity this actor plays (what the scripts' `MOVE`, `ANIM` and `DIRA` address).
    pub entity: u8,
    /// A label for logs and tests (the entity's name in the field's script).
    pub name: String,
    pub look: Look,
    /// Where it stands when the stage scripts do not place it.
    pub start: Option<Spot>,
}

/// The character whose talk starts the scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// The script entity talked to.
    pub entity: u8,
    /// Its name in the field's script (a label; the server announces the scene NPC under it).
    pub name: String,
    /// The talk script's slot (1 in every field).
    pub slot: u8,
    /// How near a player has to stand, as the script sets it (`TALKR`, at field scale 512).
    pub talk_radius: u16,
    /// The story moments (`Var[2][0]`) the scene is offered at.
    pub moments: RangeInclusive<u16>,
}

/// A story scene (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneDef {
    /// Stable id, `<game>/<field>/<name>`.
    pub id: String,
    /// The game (`ff7`).
    pub game: String,
    /// The field the scene plays in (the key its archive spells it by).
    pub field: String,
    pub trigger: Trigger,
    /// The stage scripts, run quietly first.
    pub setup: Vec<ScriptRef>,
    pub given: Vec<Given>,
    /// What the field's own trigger does to start it.
    pub root: Vec<Step>,
    pub cast: Vec<CastMember>,
    /// `(name on the first line of a window, cast entity)`.
    pub speakers: Vec<(String, u8)>,
    /// Where the witness stands: the player's own character first, then the watchers.
    pub witness: Vec<Spot>,
    /// Where a player that is put in the scene's field (the debug preview) arrives: on the floor, within the trigger's reach.
    pub arrival: Spot,
}

impl SceneDef {
    /// Rebind every entity reference to a replacement section by its original entity name,
    /// ignoring ASCII case as field scripts can capitalize the same actor differently.
    /// Refuse missing or ambiguous names: guessing would drive the wrong actor or script.
    /// This only maps identities; it does not establish that the replacement has equivalent
    /// script slots, dialogue choices, timing, or multiplayer synchronization.
    pub fn rebind_entities(&self, original: &FieldScript<'_>, replacement: &FieldScript<'_>) -> Option<Self> {
        let map = |entity: u8| -> Option<u8> {
            let name = original.entity_name(usize::from(entity))?;
            if original.entities().iter().filter(|entry| entry.name.eq_ignore_ascii_case(name)).count() != 1 {
                return None;
            }
            let mut matches = replacement.entities().iter().enumerate().filter(|(_, entry)| entry.name.eq_ignore_ascii_case(name));
            let index = matches.next()?.0;
            if matches.next().is_some() { return None; }
            u8::try_from(index).ok()
        };
        let mut def = self.clone();
        def.trigger.entity = map(def.trigger.entity)?;
        for script in &mut def.setup { script.entity = map(script.entity)?; }
        for step in &mut def.root {
            match step {
                Step::Await(script) | Step::Start(script) => script.entity = map(script.entity)?,
                Step::StartMain(entity) => *entity = map(*entity)?,
                Step::SetBit { .. } => {},
            }
        }
        for member in &mut def.cast { member.entity = map(member.entity)?; }
        for (_, entity) in &mut def.speakers { *entity = map(*entity)?; }
        Some(def)
    }
    /// Whether the scene is offered at story moment `moment`.
    pub fn applies_at(&self, moment: u16) -> bool {
        self.trigger.moments.contains(&moment)
    }

    /// Whether the scene has already begun for `story`: every mark its own root turns on first (the field's way of saying that
    /// this conversation has started, which the field's own trigger waits to be clear before it starts it) is on in the story.
    /// A mark the story model does not carry ([`crate::story::TRACKED_VARS`]) cannot be read back, so a scene whose marks are
    /// not carried is never begun (it is offered each time), and neither is one that marks nothing.
    pub fn begun_in(&self, story: &StoryState) -> bool {
        let mut marks = self.root.iter().filter_map(|step| match step {
            Step::SetBit { bank, address, bit } => Some((*bank, *address, *bit)),
            _ => None,
        });
        let Some(first) = marks.next() else { return false };
        std::iter::once(first).chain(marks).all(|(bank, address, bit)| story.known_bit(bank, address, bit) == Some(true))
    }

    /// Whether the scene is on offer for `story`: at its moment and not begun yet (module docs: a story scene plays once per
    /// story, as far as the story carries its mark).
    pub fn offered_to(&self, story: &StoryState) -> bool {
        self.applies_at(story.moment) && !self.begun_in(story)
    }

    /// The same scene with another cast (the entities the script drives stay; the actors that play them change).
    #[must_use]
    pub fn with_cast(mut self, cast: Vec<CastMember>) -> Self {
        self.cast = cast;
        self
    }

    /// The cast member playing script entity `entity`.
    pub fn member(&self, entity: u8) -> Option<&CastMember> {
        self.cast.iter().find(|m| m.entity == entity)
    }

    /// The cast entity a window's first-line name stands for (case does not matter).
    pub fn speaker_entity(&self, label: &str) -> Option<u8> {
        let label = label.trim();
        self.speakers.iter().find(|(name, _)| name.eq_ignore_ascii_case(label)).map(|(_, entity)| *entity)
    }
}

/// The id of the proof scene: Cloud and Tifa at the bar of 7th Heaven.
pub const PROOF_SCENE_ID: &str = "ff7/mds7pb_1/tifa-bar";

/// The field of the proof scene.
pub const PROOF_FIELD: &str = "mds7pb_1";

/// The director sequence immediately after the first reactor escape.
pub const POST_ESCAPE_SCENE_ID: &str = "ff7/md8_4/post-escape";
pub const STATION_SCENE_ID: &str = "ff7/md8_1/post-escape-station";

/// Field-authored cast and director scripts at moment 33. Not registered until the arrival
/// trigger and subsequent field transition are ready to serve together.
pub fn post_escape_scene() -> SceneDef {
    let cast = [(1, "cloud", "aaaa", "aafe", 19, 375, 5, 224),
                (2, "barret", "acgd", "adcb", -100, 493, 5, 0),
                (4, "biggs", "bwfd", "aafe", -41, 699, 7, 0),
                (5, "jessie", "axdc", "aafe", -65, 95, 1, 192),
                (6, "wedge", "bxbe", "aafe", 68, 245, 3, 0)]
        .into_iter().map(|(entity, name, hrc, idle, x, y, triangle, direction)| CastMember {
            entity, name: name.to_string(), look: Look::Model { hrc: hrc.to_string(), idle: idle.to_string() },
            start: Some(Spot { x, y, triangle, direction }),
        }).collect();
    SceneDef {
        id: POST_ESCAPE_SCENE_ID.to_string(), game: "ff7".to_string(), field: "md8_4".to_string(),
        trigger: Trigger { entity: 0, name: "dic".to_string(), slot: 0, talk_radius: 0, moments: 33..=33 },
        setup: vec![], given: vec![],
        root: vec![Step::StartMain(1), Step::StartMain(2), Step::StartMain(4), Step::StartMain(5), Step::StartMain(6), Step::StartMain(0)],
        cast, speakers: vec![("Cloud".into(), 1), ("Barret".into(), 2), ("Biggs".into(), 4), ("Jessie".into(), 5), ("Wedge".into(), 6)],
        witness: vec![Spot { x: -133, y: 144, triangle: 0, direction: 0 }],
        arrival: Spot { x: 82, y: 267, triangle: 3, direction: 0 },
    }
}

/// The following station director. This data is not registered until its transition and
/// presentation gaps have been addressed.
pub fn station_scene() -> SceneDef {
    let mut def = post_escape_scene();
    def.id = STATION_SCENE_ID.to_string();
    def.field = "md8_1".to_string();
    def.root = vec![Step::StartMain(0)];
    for (member, entity) in def.cast.iter_mut().zip([1u8, 2, 3, 4, 5]) {
        member.entity = entity;
        member.start = Some(Spot { x: if entity == 1 { 1577 } else { 1630 }, y: -5357,
            triangle: if entity == 1 { 10 } else { 15 }, direction: 192 });
    }
    def.speakers = vec![("Cloud".into(), 1), ("Barret".into(), 2), ("Biggs".into(), 3), ("Jessie".into(), 4), ("Wedge".into(), 5)];
    def.arrival = Spot { x: 1629, y: -5357, triangle: 15, direction: 192 };
    // Its witness is deliberately absent until a visible, walkable spot is verified.
    def.witness.clear();
    def
}

/// Script entities and slots of the proof field (measured on the owned Steam copy: `tests/scene_real.rs` checks them).
pub mod proof {
    /// The `ID` entity whose script 0 loop starts the scene.
    pub const ID: u8 = 8;
    pub const CLOUD: u8 = 18;
    pub const TIFA: u8 = 20;
    /// Tifa's talk script.
    pub const TALK: u8 = 1;
    /// Tifa's script that is the conversation at the counter.
    pub const COUNTER_TALK: u8 = 12;
    /// Cloud's script the field runs first (the lock and the camera).
    pub const CLOUD_SITS: u8 = 9;
    /// Tifa's stage script of the moment (where she stands behind the counter).
    pub const TIFA_STAGE: u8 = 7;
}

/// **The proof scene: Cloud and Tifa at the bar in 7th Heaven** (field `mds7pb_1`, story moment 72).
///
/// Found in the owner's data, not invented: at moment 72 Tifa (entity 20, `tifa`, a party member's entity that the field
/// parks behind the counter with a talk radius of 150) says her talk script's invitation, and the conversation itself
/// is started by the field's `ID` entity (script 0, the loop that watches Cloud's triangle): when Cloud stands on
/// triangle 62 with flag 1 of `Var[3][212]` set and flag 2 clear, it locks the controls, sets flag 2 of
/// `Var[3][212]`, runs Cloud's script 9 to its end and starts Tifa's script 12 (the counter: she asks what he will
/// drink). That script's tail carries on through Cloud's script 9, Tifa's 13 and Cloud's 13 into the lines after the
/// drink. The scene is that chain, in that order; the real-data test `the_trigger_is_what_the_field_does` reads the
/// `ID` script and fails if any of it differs.
pub fn proof_scene() -> SceneDef {
    SceneDef {
        id: PROOF_SCENE_ID.to_string(),
        game: "ff7".to_string(),
        field: PROOF_FIELD.to_string(),
        trigger: Trigger { entity: proof::TIFA, name: "tifa".to_string(), slot: proof::TALK, talk_radius: 150, moments: 72..=72 },
        // Tifa's own stage script of this moment puts her behind the counter (`Var[3][213]` bits clear: the first visit).
        setup: vec![ScriptRef::new(proof::TIFA, proof::TIFA_STAGE)],
        // What is set when the field has run its first scenes: the director's mark (`Var[3][211]` bit 2) and the flag
        // Cloud's arrival leaves (`Var[3][212]` bit 1). The moment itself comes from the player's story.
        given: vec![Given { bank: 3, address: 211, value: 0b0000_0100 }, Given { bank: 3, address: 212, value: 0b0000_0010 }],
        root: vec![
            Step::SetBit { bank: 3, address: 212, bit: 2 },
            Step::Await(ScriptRef::new(proof::CLOUD, proof::CLOUD_SITS)),
            Step::Start(ScriptRef::new(proof::TIFA, proof::COUNTER_TALK)),
        ],
        cast: vec![
            CastMember {
                entity: proof::CLOUD,
                name: "cloud".to_string(),
                look: Look::Appearance("ff7/cloud".to_string()),
                // The stool at the counter (Cloud's script 10 puts him there when he sits).
                start: Some(Spot { x: -95, y: -144, triangle: 62, direction: 192 }),
            },
            CastMember { entity: proof::TIFA, name: "tifa".to_string(), look: Look::Appearance("ff7/tifa".to_string()), start: None },
        ],
        speakers: vec![("Cloud".to_string(), proof::CLOUD), ("Tifa".to_string(), proof::TIFA)],
        // The open floor in front of the counter, on the customers' side (Cloud's stool is at (-95, -144) on triangle 62;
        // Tifa's lane is across the counter, 170 units to the left, and the floor between them is not walkable). The three
        // stand 90 or more units apart, each facing Cloud's stool, on the walkmesh, 78 or more units from every place the
        // cast takes in the scene and 34 or more from the nearest wall, stool or table (a body's ring is 22):
        // `tests/scene_real.rs` measures that, and `examples/scene_map.rs` draws it over the field's picture. What the
        // CAMERA shows is measured by the client's real-data test (`fmv-client`, `scene::shots`): over every frame of both
        // ways through the scene no person's picture is covered more than 10% by another's, whichever of the three
        // placeholder bodies stands on a spot (the first spots tried here, 72 units from Cloud's stool, were not enough:
        // seen along the camera the stool is almost in line with them).
        witness: vec![
            Spot { x: -70, y: -70, triangle: 24, direction: 243 },
            Spot { x: 20, y: -70, triangle: 49, direction: 215 },
            Spot { x: -100, y: 60, triangle: 68, direction: 1 },
        ],
        // At the counter's edge, facing Tifa across it (she stands at (-268, -105); her talk radius is 150).
        arrival: Spot { x: -124, y: -112, triangle: 27, direction: 192 },
    }
}

/// Every scene the game knows (today the proof).
pub fn scenes() -> Vec<SceneDef> {
    vec![proof_scene()]
}

/// The scene with id `id`.
pub fn scene(id: &str) -> Option<SceneDef> {
    scenes().into_iter().find(|s| s.id == id)
}

/// The scenes that play in field `field` (the archive's spelling; case does not matter).
pub fn scenes_in_field(field: &str) -> Vec<SceneDef> {
    scenes().into_iter().filter(|s| s.field.eq_ignore_ascii_case(field)).collect()
}
