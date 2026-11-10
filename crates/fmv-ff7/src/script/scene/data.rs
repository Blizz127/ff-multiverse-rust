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
    /// For the CUTSCENE of one of the field's own story points: the story moment that point moves the story to. Such a scene
    /// is never offered by itself (no trigger character, no start on arrival, see [`SceneDef::offered_to`]): the server plays
    /// it when a player reaches that point, built against the story the player has before the step, and applies the point's
    /// own step (read from the script by the story model, not from this run) when it ends. It writes nothing of its own and
    /// sends nobody anywhere. `None` for every other scene.
    pub presents: Option<u16>,
    /// The same kind of cutscene for an existing saved bit turning on without changing the moment.
    /// `(bank, address, bit)` must be carried by the story; unknown bits never match. Mutually exclusive with `presents`.
    pub presents_bit: Option<(u8, u8, u8)>,
    /// A victory-only continuation. Reconstruct this root up to the first BATTLE of this formation, then show only
    /// its tail. Never offered on entry or before combat; the server starts it only after a matching story battle wins.
    /// It cannot save, start combat, or delay the victory's existing story step/countdown.
    pub resume_after_battle: Option<u16>,
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

    /// Whether the scene starts by itself when a player whose story it is offered to comes to stand in its field (an
    /// ARRIVAL scene: what the field's own director does from the first frame on), instead of on a press next to a
    /// character. That is what a trigger nobody can talk to means: its talk radius is 0, so no press can ever reach it and
    /// the server announces no character for it. A scene a talk starts always has a radius (the field's own `TALKR`).
    ///
    /// The field of an arrival scene must not be one of [`crate::REVIEWED_FIELDS`]: there the field's own story points are
    /// offered, and the world runs the ones that fire on entry BEFORE it judges the scene, so a point that writes the
    /// scene's moment (as `md8_1`'s does) would leave the scene never offered (`tests/scene_real.rs` holds the list to it).
    pub fn starts_on_arrival(&self) -> bool {
        self.trigger.talk_radius == 0 && !self.is_point_cutscene()
    }

    /// Whether this scene is the cutscene of the step of field `field` that takes a story from `before` to `after` (see
    /// [`SceneDef::presents`] and [`SceneDef::presents_bit`]): the field and transition match, and the scene applies at
    /// the story the step starts from.
    pub fn presents_step(&self, field: &str, before: &StoryState, after: &StoryState) -> bool {
        self.resume_after_battle.is_none() && self.matches_step(field, before, after)
    }

    /// Whether a winning battle has the formation and story transition this continuation belongs to.
    pub fn follows_battle_step(&self, field: &str, before: &StoryState, after: &StoryState, formation: u16) -> bool {
        self.resume_after_battle == Some(formation) && self.matches_step(field, before, after)
    }

    fn matches_step(&self, field: &str, before: &StoryState, after: &StoryState) -> bool {
        if !self.field.eq_ignore_ascii_case(field) || !self.applies_at(before.moment) { return false; }
        match (self.presents, self.presents_bit) {
            (Some(moment), None) => after.moment == moment && after.moment > before.moment,
            (None, Some((bank, address, bit))) => before.moment == after.moment
                && before.known_bit(bank, address, bit) == Some(false)
                && after.known_bit(bank, address, bit) == Some(true),
            _ => false,
        }
    }

    /// Whether this scene belongs to a story point instead of being offered independently.
    pub fn is_point_cutscene(&self) -> bool {
        self.presents.is_some() || self.presents_bit.is_some()
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
        !self.is_point_cutscene() && self.applies_at(story.moment) && !self.begun_in(story)
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
pub const TRAIN_SCENE_ID: &str = "ff7/tin_1/sector-seven-train";
/// Survey-only early Seventh Heaven director; not registered for play.
pub const EARLY_BAR_SCENE_ID: &str = "ff7/mds7pb_1/early-bar-survey";
/// Survey-only station arrival in Sector 7; not registered for play.
pub const SECTOR7_STATION_SCENE_ID: &str = "ff7/mds7st3/station-arrival-survey";
/// Survey-only Sector 7 intro; not registered for play.
pub const SECTOR7_INTRO_SCENE_ID: &str = "ff7/mds7/intro-survey";
/// Survey-only flower seller scene; the gil transaction is not implemented.
pub const FLOWER_STATION_SCENE_ID: &str = "ff7/md8_2/flower-survey";
/// Survey-only Sector 7 pillar-field entry; not registered for play.
pub const SECTOR7_PILLAR_SCENE_ID: &str = "ff7/mds7plr1/entry-survey";

/// Survey-only Reactor opening; never offered for live play.
pub const REACTOR_OPENING_SCENE_ID: &str = "ff7/md1stin/opening-survey";

/// Survey-only cargo train director; not registered for play.
pub const CARGO_TRAIN_SCENE_ID: &str = "ff7/cargoin/train-survey";

/// **After the escape: the team at the tunnel mouth** (field `md8_4`, story moment 33). An arrival scene
/// ([`SceneDef::starts_on_arrival`]): the field's director (`dic`, entity 0) does it by itself when the field loads at
/// moment 33. It has the five talk (four windows), sends them running and ends in the field's own `MAPJUMP` to `md8_1`,
/// where the next scene ([`station_scene`]) takes over. It writes nothing that is saved and touches no party
/// (`tests/scene_real.rs`, `the_post_escape_scene_changes_nothing_that_is_saved`); at any other moment the director does
/// nothing. The cast is the field's own five characters with the models its loader names, each started where its own
/// script places it at this moment; the root starts the main part of each of them and of the director, as the field does.
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
        // Known gap: the field's camera looks along the only open floor, so no spot of it shows a witness clear of the cast
        // (621 places scanned by `fmv-client`'s `scene::shots::offscreen_survey_post_escape_witness`). This one keeps the
        // walkmesh rule (71 units from the cast, 76 from the edge) and stays out of the way, behind the rock: the body is
        // barely visible.
        witness: vec![Spot { x: -133, y: 144, triangle: 0, direction: 0 }],
        arrival: Spot { x: 82, y: 267, triangle: 3, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// **After the escape: the team splits up on the street** (field `md8_1`, story moment 33). An arrival scene, like
/// [`post_escape_scene`], which ends in the jump to this field: the director (`dic`, entity 0) has the five jump down
/// from where the player arrives and run along the street, four windows, then takes Barret out of the party (`PRTYM 1`),
/// writes **moment 36** and restores the party (`HMPMAX3`). The party change is carried by the story's party table (its
/// row of moment 36 is Cloud alone: `crate::story::CANON_PARTIES`) and the restore is what every battle of this server
/// starts with anyway (`tests/scene_real.rs`, `the_station_scene_takes_barret_out_of_the_party_and_...`). The opening
/// movie the director plays first is skipped.
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
    // The witness stands at the left end of the street, facing the group (which talks near (1347, -5270)): 224 walkmesh
    // units from every place and walk of the cast, 123 from the nearest edge of the floor, and its whole body in view in
    // every picture of the scene (the cast's Wedge runs past in front of it for 4 of 105 steps). Measured by `fmv-client`'s
    // `scene::shots::offscreen_survey_station_witness` on the owner's field; the floor nearer the arrival is crossed by the
    // cast's run, which is why it is this far along the street.
    def.witness = vec![Spot { x: 1074, y: -5183, triangle: 28, direction: 52 }];
    def
}

/// The train's arrival director at moment 48. Not registered for live play: presentation,
/// preceding travel, and persistence effects elsewhere on the route still need review.
pub fn train_scene() -> SceneDef {
    let cast = [
        (30, "cloud", "aaaa", "acfe"), (31, "barret", "acgd", "adcb"),
        (33, "biggs", "bwfd", "acfe"), (34, "jessie", "axdc", "acfe"),
        (35, "wedge", "bxbe", "acfe"), (36, "guard", "azhe", "acfe"),
        (37, "employee", "cdja", "bdcb"), (40, "passenger", "cfha", "cgcc"),
        (41, "woman", "cbfe", "bajb"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: TRAIN_SCENE_ID.into(), game: "ff7".into(), field: "tin_1".into(),
        trigger: Trigger { entity: 17, name: "direct".into(), slot: 0, talk_radius: 0, moments: 48..=48 },
        setup: vec![ScriptRef::new(40, 0)], given: vec![], root: vec![Step::StartMain(17)], cast,
        speakers: vec![("Cloud".into(), 30), ("Barret".into(), 31), ("Biggs".into(), 33), ("Jessie".into(), 34), ("Wedge".into(), 35)],
        // Deliberately absent until a visible walkmesh location has been checked.
        witness: vec![], arrival: Spot { x: -2, y: 608, triangle: 29, direction: 252 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Exploratory early bar director at moment 63. This is not registered: party/menu effects,
/// persistent flags, timing, witness placement and the route into this field are not ready.
pub fn early_bar_scene() -> SceneDef {
    let mut def = proof_scene();
    def.id = EARLY_BAR_SCENE_ID.into();
    def.trigger = Trigger { entity: 7, name: "direct".into(), slot: 1, talk_radius: 0, moments: 63..=63 };
    def.setup = vec![ScriptRef::new(10, 1)];
    def.given.clear();
    def.root = vec![Step::Await(ScriptRef::new(7, 1))];
    for (entity, name, hrc, idle) in [
        (23, "aval", "bwfd", "acfe"), (24, "avas", "bxbe", "acfe"),
        (25, "marine", "cyae", "bnac"), (26, "camera", "cyif", "acfe"),
        (28, "cash", "czgb", "czge"), (29, "flower", "czgf", "czhc"),
    ] {
        def.cast.push(CastMember { entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None });
    }
    def.witness.clear();
    def
}

/// The id of the team's talk at the reactor's gate (the cutscene of `md1_1`'s story point to 7).
pub const TEAM_TALK_SCENE_ID: &str = "ff7/md1_1/team-talk";

/// **The team at the reactor's gate** (field `md1_1`): the cutscene of the field's story point that moves the story to 7
/// ([`SceneDef::presents`]). The field's director (`dir`, entity 0) watches the triangle the player's character stands on;
/// when Cloud reaches triangle 62 with the story before 7 it locks the controls, writes 7 and plays the team's talk: twelve
/// windows (Biggs, Jessie, Cloud and Barret), the door, and its own jump on to the next field. As a cutscene the scene is
/// played for the player who reaches the point, built against its story before the step: the stage scripts are the init
/// part of every entity's script 0 (what the field does when it loads), the root starts the director's main loop, and Cloud
/// starts on triangle 62 (its centre), where the director's `GETAI` finds him. The step itself is the story point's,
/// applied when the scene ends (`tests/scene_real.rs` holds the scene's own write to it); the jump is not made.
pub fn team_talk_scene() -> SceneDef {
    let cast = [
        (1, "cl", "aaaa", "acfe"), (2, "ba", "acgd", "adcb"), (3, "av_m", "bwfd", "acfe"), (4, "av_l", "axdc", "acfe"),
        (5, "av_s", "bxbe", "acfe"), (6, "drL", "bxjb", "bxjf"), (7, "drR", "bxjb", "bxjf"),
    ]
    .into_iter()
    .map(|(entity, name, hrc, idle)| CastMember {
        entity,
        name: name.into(),
        look: Look::Model { hrc: hrc.into(), idle: idle.into() },
        start: (entity == 1).then_some(Spot { x: 1120, y: 1468, triangle: 62, direction: 0 }),
    })
    .collect();
    SceneDef {
        id: TEAM_TALK_SCENE_ID.into(),
        game: "ff7".into(),
        field: "md1_1".into(),
        trigger: Trigger { entity: 0, name: "dir".into(), slot: 0, talk_radius: 0, moments: 0..=6 },
        setup: (0..12).map(|e| ScriptRef::new(e, 0)).collect(),
        given: vec![],
        root: vec![Step::StartMain(0)],
        cast,
        speakers: vec![("Cloud".into(), 1), ("Barret".into(), 2), ("Biggs".into(), 3), ("Jessie".into(), 4), ("Wedge".into(), 5)],
        // Known gap until a spot is measured: the player stands where it reached the point.
        witness: vec![],
        arrival: Spot { x: 1120, y: 1468, triangle: 62, direction: 0 },
        presents: Some(7), resume_after_battle: None, presents_bit: None,
    }
}

/// The id of Barret's talk at the reactor's door (the cutscene of `nmkin_1`'s story line to 11).
pub const REACTOR_DOOR_SCENE_ID: &str = "ff7/nmkin_1/reactor-door";

/// **Barret and Cloud at the reactor's door** (field `nmkin_1`): the cutscene of the field's story LINE that moves the story
/// to 11 ([`SceneDef::presents`]). The line (`evb`, entity 12, across the corridor at y 2166) runs its script 3 when the
/// player's character walks onto it with the story before 11: it writes 11, has Cloud and Barret walk and talk (five
/// windows), shows the door's window and sets the party back to Cloud and Barret (`PRTYE`, the party the story already
/// has). As a cutscene the root is that script, the stage scripts the init part of every named entity's script 0 (Barret
/// stands at the door), and Cloud starts on the line, on triangle 25, facing the door; the step is the line's own, applied
/// when the scene ends (`tests/scene_real.rs` holds the scene's own write to it).
pub fn reactor_door_scene() -> SceneDef {
    let cast = [(3, "cl", "aaaa", "acfe"), (4, "ba", "acgd", "adcb")]
        .into_iter()
        .map(|(entity, name, hrc, idle)| CastMember {
            entity,
            name: name.into(),
            look: Look::Model { hrc: hrc.into(), idle: idle.into() },
            start: (entity == 3).then_some(Spot { x: -704, y: 2150, triangle: 25, direction: 128 }),
        })
        .collect();
    SceneDef {
        id: REACTOR_DOOR_SCENE_ID.into(),
        game: "ff7".into(),
        field: "nmkin_1".into(),
        trigger: Trigger { entity: 12, name: "evb".into(), slot: 3, talk_radius: 0, moments: 0..=10 },
        // The inits of the entities that take part or stage the corridor. Left out: the door's four unnamed pieces and the
        // bomb's two timers (`time`, `timeo`), which take no part; a scene is bound to another edition of the script by its
        // entities' names, and Echo-S's edition adds a `Time` entity that a case-blind match cannot tell from `time`.
        setup: [0u8, 3, 4, 9, 10, 11, 12, 13, 14].into_iter().map(|e| ScriptRef::new(e, 0)).collect(),
        given: vec![],
        root: vec![Step::Await(ScriptRef::new(12, 3))],
        cast,
        speakers: vec![("Cloud".into(), 3), ("Barret".into(), 4)],
        // Known gap until a spot is measured: the player stands where it crossed the line.
        witness: vec![],
        arrival: Spot { x: -704, y: 2150, triangle: 25, direction: 128 },
        presents: Some(11), resume_after_battle: None, presents_bit: None,
    }
}

/// The id of the talk before the Guard Scorpion (the cutscene of `nmkin_5`'s story point to 27, which starts the battle).
pub const GUARD_SCORPION_SCENE_ID: &str = "ff7/nmkin_5/guard-scorpion";

/// **Barret and Cloud before the Guard Scorpion** (field `nmkin_5`): the cutscene of the field's story point that starts the
/// boss fight and moves the story to 27 when it is WON ([`SceneDef::presents`]). The director (`dir`, entity 1) watches the
/// triangle Cloud stands on; on triangle 2 with the story before 27 it runs Barret's script 3: the talk at the core (eleven
/// windows) and then the battle, where the scene ends. As a cutscene the root is the director's main loop, the stage
/// scripts the inits of the director (it names the triangle it watches), Cloud and Barret, and Cloud starts on triangle 2.
/// When the run ends the server starts the point's battle as before, and the step is paid when it is won; the lines after
/// the battle are played by [`guard_scorpion_victory_scene`] after a win.
pub fn guard_scorpion_scene() -> SceneDef {
    let cast = [(3, "cl", "aaaa", "acfe"), (4, "ba", "acgd", "adcb")]
        .into_iter()
        .map(|(entity, name, hrc, idle)| CastMember {
            entity,
            name: name.into(),
            look: Look::Model { hrc: hrc.into(), idle: idle.into() },
            start: (entity == 3).then_some(Spot { x: -67, y: -1571, triangle: 2, direction: 0 }),
        })
        .collect();
    SceneDef {
        id: GUARD_SCORPION_SCENE_ID.into(),
        game: "ff7".into(),
        field: "nmkin_5".into(),
        trigger: Trigger { entity: 1, name: "dir".into(), slot: 0, talk_radius: 0, moments: 0..=26 },
        setup: [1u8, 3, 4].into_iter().map(|e| ScriptRef::new(e, 0)).collect(),
        given: vec![],
        root: vec![Step::StartMain(1)],
        cast,
        speakers: vec![("Cloud".into(), 3), ("Barret".into(), 4)],
        // Known gap until a spot is measured: the player stands where it reached the point.
        witness: vec![],
        arrival: Spot { x: -67, y: -1571, triangle: 2, direction: 0 },
        presents: Some(27), resume_after_battle: None, presents_bit: None,
    }
}

/// The two windows after the Guard Scorpion, from the same Barret script that requested the fight.
pub const GUARD_SCORPION_VICTORY_SCENE_ID: &str = "ff7/nmkin_5/guard-scorpion-victory";

pub fn guard_scorpion_victory_scene() -> SceneDef {
    let mut def = guard_scorpion_scene();
    def.id = GUARD_SCORPION_VICTORY_SCENE_ID.into();
    // Keep the director's infinite field loop and timer window out of this bounded continuation. Barret awaits
    // Cloud's script 6, which reaches BATTLE 324; after victory it resumes, returns to Barret, and both finish.
    def.root = vec![Step::Await(ScriptRef::new(4, 3))];
    def.resume_after_battle = Some(324);
    def
}

/// Jessie's rescue on the way out of the reactor: the existing saved bit, not another moment.
pub const JESSIE_RESCUE_SCENE_ID: &str = "ff7/nmkin_3/jessie-rescue";

pub fn jessie_rescue_scene() -> SceneDef {
    use crate::story::{FREED_BIT, REACTOR_STATE};
    let arrival = Spot { x: -183, y: 1271, triangle: 6, direction: 0 };
    let cast = [(3, "cl", "aaaa", "acfe"), (4, "av_j", "axdc", "acfe")]
        .into_iter().map(|(entity, name, hrc, idle)| CastMember {
            entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() },
            start: (entity == 3).then_some(arrival),
        }).collect();
    SceneDef {
        id: JESSIE_RESCUE_SCENE_ID.into(), game: "ff7".into(), field: "nmkin_3".into(),
        trigger: Trigger { entity: 4, name: "av_j".into(), slot: 1, talk_radius: 0, moments: 27..=27 },
        setup: [3, 4].into_iter().map(|entity| ScriptRef::new(entity, 0)).collect(),
        given: vec![], root: vec![Step::Await(ScriptRef::new(4, 1))], cast,
        speakers: vec![("Cloud".into(), 3), ("Jessie".into(), 4)], witness: vec![], arrival,
        presents: None, resume_after_battle: None,
        presents_bit: Some((REACTOR_STATE.bank, REACTOR_STATE.address, FREED_BIT)),
    }
}

/// Jessie's first ladder explanation, before the existing talk point advances the story to 14.
pub const JESSIE_LADDER_SCENE_ID: &str = "ff7/nmkin_3/jessie-ladder";

pub fn jessie_ladder_scene() -> SceneDef {
    let arrival = Spot { x: 100, y: 1910, triangle: 27, direction: 64 };
    let cast = [(4, "av_j", "axdc", "acfe"), (5, "po0", "bygf", "byhc")]
        .into_iter().map(|(entity, name, hrc, idle)| CastMember {
            entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() },
            start: None,
        }).collect();
    SceneDef {
        id: JESSIE_LADDER_SCENE_ID.into(), game: "ff7".into(), field: "nmkin_3".into(),
        trigger: Trigger { entity: 4, name: "av_j".into(), slot: 1, talk_radius: 0, moments: 0..=13 },
        setup: [3, 4, 5].into_iter().map(|entity| ScriptRef::new(entity, 0)).collect(),
        given: vec![], root: vec![Step::Await(ScriptRef::new(4, 1))], cast,
        speakers: vec![("Jessie".into(), 4)],
        // The narrow upper platform has no safe automatic sample. This spot was measured against the
        // walkmesh and Jessie's entire path; extra watchers retain their actual positions.
        witness: vec![Spot { x: 85, y: 1910, triangle: 42, direction: 64 }], arrival,
        presents: Some(14), resume_after_battle: None, presents_bit: None,
    }
}

/// The AVALANCHE members run ahead through the reactor approach before the existing entry step to 8.
pub const RUN_AHEAD_SCENE_ID: &str = "ff7/md1_2/run-ahead";

pub fn run_ahead_scene() -> SceneDef {
    let cast = [(2, "av_b", "bwfd"), (3, "av_c", "axdc"), (4, "av_w", "bxbe")]
        .into_iter().map(|(entity, name, hrc)| CastMember {
            entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: "acfe".into() }, start: None,
        }).collect();
    SceneDef {
        id: RUN_AHEAD_SCENE_ID.into(), game: "ff7".into(), field: "md1_2".into(),
        trigger: Trigger { entity: 2, name: "av_b".into(), slot: 0, talk_radius: 0, moments: 0..=7 },
        setup: [2, 3, 4].into_iter().map(|entity| ScriptRef::new(entity, 0)).collect(),
        given: vec![],
        // All three mains start together, as in the field. Biggs advances the moment; Jessie's and
        // Wedge's authored waits and paths finish before the scene releases its witness.
        root: [2, 3, 4].into_iter().map(Step::StartMain).collect(), cast,
        speakers: vec![], witness: vec![],
        arrival: Spot { x: 3560, y: 30579, triangle: 58, direction: 128 },
        presents: Some(8), resume_after_battle: None, presents_bit: None,
    }
}

/// Jessie's descent from the lift, before the existing entry step to 13.
pub const JESSIE_DESCENT_SCENE_ID: &str = "ff7/nmkin_2/jessie-descent";

pub fn jessie_descent_scene() -> SceneDef {
    SceneDef {
        id: JESSIE_DESCENT_SCENE_ID.into(), game: "ff7".into(), field: "nmkin_2".into(),
        trigger: Trigger { entity: 4, name: "av_j".into(), slot: 0, talk_radius: 0, moments: 0..=12 },
        setup: vec![ScriptRef::new(4, 0)], given: vec![], root: vec![Step::StartMain(4)],
        cast: vec![CastMember { entity: 4, name: "av_j".into(), look: Look::Model { hrc: "axdc".into(), idle: "acfe".into() }, start: None }],
        speakers: vec![], witness: vec![],
        arrival: Spot { x: -682, y: 308, triangle: 92, direction: 64 },
        presents: Some(13), resume_after_battle: None, presents_bit: None,
    }
}

/// Offline director survey at moment 51. The route, witness, field effects and party state
/// have not been verified for live play, so this definition is deliberately unregistered.
pub fn sector7_station_scene() -> SceneDef {
    let cast = [
        (11, "cloud", "aaaa", "aafe"), (12, "ballet", "acgd", "adcb"),
        (14, "aval", "bwfd", "acfe"), (15, "avaf", "bxbe", "acfe"),
        (16, "avaw", "axdc", "acfe"), (17, "guard", "azhe", "badc"),
        (18, "man1", "cfha", "cgcc"), (19, "obasan1", "coad", "coab"),
        (20, "woman1", "cbfe", "bajb"), (22, "child", "cpca", "bnac"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: SECTOR7_STATION_SCENE_ID.into(), game: "ff7".into(), field: "mds7st3".into(),
        trigger: Trigger { entity: 1, name: "produce".into(), slot: 0, talk_radius: 0, moments: 51..=51 },
        setup: vec![], given: vec![], root: vec![Step::StartMain(1)], cast,
        speakers: vec![("Cloud".into(), 11), ("Barret".into(), 12)],
        witness: vec![], arrival: Spot { x: -2891, y: 3392, triangle: 26, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Offline intro survey. The producer assumes persistent flags and changes other untracked
/// flags; this data must not be registered until those effects and presentation are ready.
pub fn sector7_intro_scene() -> SceneDef {
    let cast = [
        (22, "ballet", "acgd", "adcb"), (23, "tifa", "aagb", "abcd"),
        (24, "avaw", "axdc", "acfe"), (25, "avas", "bxbe", "acfe"),
        (26, "aval", "bwfd", "acfe"), (27, "man1", "cjif", "cgcc"),
        (28, "oyaji1", "asbf", "acfe"), (29, "woman1", "cwed", "coab"),
        (30, "child1", "cgda", "bvcc"), (31, "obasan", "coad", "coab"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: SECTOR7_INTRO_SCENE_ID.into(), game: "ff7".into(), field: "mds7".into(),
        trigger: Trigger { entity: 7, name: "produce".into(), slot: 0, talk_radius: 0, moments: 63..=63 },
        setup: vec![], given: vec![], root: vec![Step::StartMain(7)], cast, speakers: vec![],
        witness: vec![], arrival: Spot { x: 1641, y: -328, triangle: 109, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Offline survey of the flower seller's director. Unregistered until persistent flags,
/// once-only currency effects, presentation, witness and preceding route are implemented.
pub fn flower_station_scene() -> SceneDef {
    let cast = [
        (1, "cloud", "aaaa", "aafe"), (2, "earith", "cahc", "cbdc"),
        (3, "panpi1", "cbfe", "caed"), (4, "panpi2", "ccbc", "caed"),
        (5, "panpi3", "ccbc", "caed"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: FLOWER_STATION_SCENE_ID.into(), game: "ff7".into(), field: "md8_2".into(),
        trigger: Trigger { entity: 0, name: "dic".into(), slot: 0, talk_radius: 0, moments: 36..=36 },
        setup: vec![], given: vec![], root: vec![Step::StartMain(0)], cast,
        speakers: vec![("Cloud".into(), 1), ("Aerith".into(), 2)],
        witness: vec![], arrival: Spot { x: -3445, y: 21419, triangle: 34, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Offline opening director survey. It includes initialization, an unimplemented movie and a
/// subsequent battle trigger; never register it as a live or preview scene in this form.
pub fn reactor_opening_scene() -> SceneDef {
    let cast = [
        (1, "cl", "aaaa", "acfe"), (2, "ba", "acgd", "adcb"),
        (4, "hei0", "bwab", "acfe"), (5, "hei1", "bwab", "acfe"),
        (6, "av_m", "bwfd", "acfe"), (7, "av_l", "axdc", "acfe"),
        (8, "av_s", "bxbe", "acfe"), (9, "gu0", "azhe", "aafe"),
        (10, "gu1", "azhe", "aafe"), (11, "guadd", "azhe", "aafe"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: REACTOR_OPENING_SCENE_ID.into(), game: "ff7".into(), field: "md1stin".into(),
        trigger: Trigger { entity: 0, name: "dir".into(), slot: 0, talk_radius: 0, moments: 0..=0 },
        setup: vec![], given: vec![], root: vec![Step::StartMain(0)], cast, speakers: vec![],
        witness: vec![], arrival: Spot { x: 3655, y: 27432, triangle: 25, direction: 128 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Offline cargo-train director survey at moment 39. The incoming transition, audiovisual
/// effects and witnesses have not been validated; this is not registered for live play.
pub fn cargo_train_scene() -> SceneDef {
    let cast = [
        (11, "cloud", "aaaa", "acfe"), (12, "ballet", "acgd", "adcb"),
        (13, "aval", "bwfd", "acfe"), (14, "avaf", "bxbe", "acfe"),
        (15, "avaw", "axdc", "acfe"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: CARGO_TRAIN_SCENE_ID.into(), game: "ff7".into(), field: "cargoin".into(),
        trigger: Trigger { entity: 6, name: "direct".into(), slot: 0, talk_radius: 0, moments: 39..=39 },
        setup: (11..=15).map(|e| ScriptRef::new(e, 0)).collect(), given: vec![],
        root: vec![Step::StartMain(6)], cast,
        speakers: vec![("Cloud".into(), 11), ("Barret".into(), 12)],
        witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Offline pillar-field entry survey. Its branches depend on and change uncarried flags;
/// the definition is not registered for live play.
pub fn sector7_pillar_scene() -> SceneDef {
    let cast = [
        (12, "ballet", "acgd", "adcb"), (14, "aval", "bwfd", "acfe"),
        (15, "avaf", "bxbe", "acfe"), (16, "avaw", "axdc", "acfe"),
        (20, "man1", "cmif", "acfe"),
    ].into_iter().map(|(entity, name, hrc, idle)| CastMember {
        entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: idle.into() }, start: None,
    }).collect();
    SceneDef {
        id: SECTOR7_PILLAR_SCENE_ID.into(), game: "ff7".into(), field: "mds7plr1".into(),
        trigger: Trigger { entity: 1, name: "produce".into(), slot: 0, talk_radius: 0, moments: 63..=63 },
        setup: vec![], given: vec![], root: vec![Step::StartMain(1)], cast, speakers: vec![],
        witness: vec![], arrival: Spot { x: 378, y: -264, triangle: 65, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
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
        arrival: Spot { x: -124, y: -112, triangle: 27, direction: 192 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

/// Every scene the game offers, in the order a server numbers their triggers by (a scene is added at the END, so the
/// ones before it keep their numbers): the proof, the two the escape from the first reactor ends in, and the cutscenes of the
/// reactor's own story points.
pub fn scenes() -> Vec<SceneDef> {
    vec![proof_scene(), post_escape_scene(), station_scene(), team_talk_scene(), reactor_door_scene(), guard_scorpion_scene(), guard_scorpion_victory_scene(), jessie_rescue_scene(), jessie_ladder_scene(), run_ahead_scene(), jessie_descent_scene()]
}

/// Whether `field` (the archive's spelling; case does not matter) is the field of a scene the game offers: a place a
/// story scene may send its players to, since the story goes on there by a scene of its own.
pub fn is_scene_field(field: &str) -> bool {
    scenes().iter().any(|s| s.field.eq_ignore_ascii_case(field))
}

/// Where an account comes into the world when its story has left the first field behind for good (see [`STORY_ENTRIES`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoryEntry {
    /// The first story moment (`Var[2][0]`) this entry holds for, until the next row's.
    pub from_moment: u16,
    /// The field (the key its archive spells it by).
    pub field: &'static str,
    /// Where the account stands there.
    pub spot: Spot,
}

/// **Where a story that has left the reactor comes into the world.** Everyone joins in `md1stin` and walks from there;
/// that is right for as long as the way the story took is still a way a player can walk. Two story scenes end that:
///
/// * **From moment 33** (the reactor is gone) the story is outside it, in `md8_4`, where the first scene after the
///   escape starts by itself. The spot is the arrival the field's own gateway from `nrthmk` names, which is where a
///   player that walked out of the reactor stands.
/// * **From moment 36** the story is in `md8_1`: the scene of `md8_4` ends in a script jump there, a jump only that
///   scene makes, and `md8_1` has no way back. Without this row an account that left the game at moment 36 or later
///   would join in `md1stin` and could never reach the story again. The spot is the arrival that jump names.
///
/// A row is added with each scene that takes the story somewhere no walk leads back from, and names a place the game's
/// own data puts the player at (never an invented one). Rows ascend by moment. `tests/scene_real.rs` holds every row to
/// the owner's data: the field exists, the spot is on the triangle it names, and that triangle is in the part of the
/// floor the field's ways out are in.
pub const STORY_ENTRIES: &[StoryEntry] = &[
    StoryEntry { from_moment: 33, field: "md8_4", spot: Spot { x: 82, y: 267, triangle: 3, direction: 0 } },
    StoryEntry { from_moment: 36, field: "md8_1", spot: Spot { x: 1629, y: -5357, triangle: 15, direction: 192 } },
];

/// The entry of a story at `moment` ([`STORY_ENTRIES`]): the row with the greatest `from_moment` that is not past it.
/// `None` for a story that is still where everyone joins.
pub fn story_entry(moment: u16) -> Option<StoryEntry> {
    STORY_ENTRIES.iter().rev().find(|row| row.from_moment <= moment).copied()
}

/// The scene with id `id`.
pub fn scene(id: &str) -> Option<SceneDef> {
    scenes().into_iter().find(|s| s.id == id)
}

/// The scenes that play in field `field` (the archive's spelling; case does not matter).
pub fn scenes_in_field(field: &str) -> Vec<SceneDef> {
    scenes().into_iter().filter(|s| s.field.eq_ignore_ascii_case(field)).collect()
}
