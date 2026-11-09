//! Where the game's own conversations come from: the talk script of the character the player stands
//! next to, in the field script of this machine's own FF7 data (`fmv_ff7::script::dialogue`).
//!
//! The server announces an area's characters as NPCs (`NpcSnapshot`: the script entity's name, a
//! model, a position) and nothing about their scripts, and no protocol carries dialogue: every
//! client reads the text from its own copy of the game. So the client finds, in the field's script,
//! the entity an NPC stands for by its name and where it stands
//! ([`fmv_ff7::script::dialogue::find_character`]; names are labels, so the position decides), and
//! runs its talk script.
//!
//! **Story.** The branches of a talk script depend on story variables. They are read through the
//! [`StoryVars`] hook the controller was given ([`Ff7Conversations::set_story`]): the start of a new
//! game until something better is plugged in. A server-fed hook that mirrors a real player's progress
//! goes there, and nothing else in the client changes.

use super::Conversations;
use crate::state::NpcView;
use fmv_ff7::script::dialogue::{characters, find_character, Character, MessageSource, StartOfGame, StoryVars, TalkSession};
use fmv_ff7::script::FieldScript;
use fmv_ff7::Ff7Adapter;
use std::sync::Arc;

/// A source of the story hook: called once for every conversation.
pub type StoryFactory = Box<dyn Fn() -> Box<dyn StoryVars>>;

/// The field script of one area, kept while the player is in it.
struct Loaded {
    key: String,
    /// The script section's bytes (a conversation owns a copy); `None` when the area has none that
    /// can be read.
    section: Option<Vec<u8>>,
    characters: Vec<Character>,
}

/// The conversations of this machine's own FF7 fields.
pub struct Ff7Conversations {
    ff7: Option<Arc<Ff7Adapter>>,
    area: Option<Loaded>,
    story: StoryFactory,
    /// Counts the conversations: the seed of the next one's `RANDOM`, so that a character who
    /// speaks at random does not say the same thing every time.
    started: u32,
}

/// The biggest script section read (a real one is at most 60 544 bytes).
const MAX_SECTION_BYTES: usize = 1 << 20;

impl Ff7Conversations {
    /// Conversations over `ff7` (`None`: this machine has no FF7 data, nobody has anything to say),
    /// with the story at the start of a new game.
    pub fn new(ff7: Option<Arc<Ff7Adapter>>) -> Self {
        Self { ff7, area: None, story: Box::new(|| Box::new(StartOfGame)), started: 0 }
    }

    /// The script of `area`, loaded now if it is not the one held.
    fn loaded(&mut self, area: &str) -> Option<&Loaded> {
        let ff7 = self.ff7.as_ref()?;
        if self.area.as_ref().is_none_or(|l| l.key != area) {
            let (section, chars) = match ff7.load_field(area) {
                Ok(field) => match FieldScript::from_field(field.field_file()) {
                    Ok(script) if script.bytes().len() <= MAX_SECTION_BYTES => (Some(script.bytes().to_vec()), characters(&script)),
                    _ => (None, Vec::new()),
                },
                Err(_) => (None, Vec::new()),
            };
            self.area = Some(Loaded { key: area.to_string(), section, characters: chars });
        }
        self.area.as_ref()
    }

    /// The script entity the NPC stands for, with the script section.
    fn character(&mut self, area: &str, npc: &NpcView) -> Option<(Vec<u8>, Character)> {
        let loaded = self.loaded(area)?;
        let section = loaded.section.as_ref()?;
        let found = find_character(&loaded.characters, &npc.name, npc.pos.x, npc.pos.y)?;
        Some((section.clone(), found.clone()))
    }
}

impl Conversations for Ff7Conversations {
    fn talk_radius(&mut self, area: &str, npc: &NpcView) -> f32 {
        let Some(loaded) = self.loaded(area) else { return 0.0 };
        find_character(&loaded.characters, &npc.name, npc.pos.x, npc.pos.y).map_or(0.0, |c| c.talk_radius)
    }

    fn start(&mut self, area: &str, npc: &NpcView) -> Option<Box<dyn MessageSource>> {
        let (section, character) = self.character(area, npc)?;
        self.started = self.started.wrapping_add(1);
        let session = TalkSession::talk(section, character.entity, (self.story)(), self.started).ok()?;
        Some(Box::new(session))
    }

    fn set_story(&mut self, story: StoryFactory) {
        self.story = story;
    }
}
