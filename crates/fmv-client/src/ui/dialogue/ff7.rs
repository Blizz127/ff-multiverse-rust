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
//!
//! # The text layer (Echo-S)
//!
//! With the text layer on ([`crate::mods`]), a character that Echo-S's script also has (found in ITS section by the same
//! name-and-place rule) talks from Echo-S's section: its text, its flow and its dialog numbers, which are also what its voice
//! files are named by. A character Echo-S does not have, or cannot talk at that point of the story, talks from the original
//! section, so nothing is ever silent or blank because of the layer, and **with the layer off every conversation is the
//! original's**. Echo-S's section is a different script from the original's (see `fmv_mods::mods`), so it is taken whole, never
//! mixed line by line with the original's. How far a character can be talked to (the radius) is gameplay and always the
//! original's.
//!
//! # The voice layer
//!
//! A conversation that runs on Echo-S's section sends a [`VoiceCue::Say`] to the audio each time a window opens (a window that
//! shows nothing is skipped by the dialogue window and is no cue) and a [`VoiceCue::Stop`] when the conversation is dropped,
//! whatever ended it. The audio finds the clip by Echo-S's own numbers and plays it ([`crate::audio`]).

use super::layout::is_blank;
use super::Conversations;
use crate::mods::{self, ModSink, VoiceCue};
use crate::state::NpcView;
use fmv_ff7::script::dialogue::{characters, find_character, Beat, Character, MessageOrigin, MessageSource, StartOfGame, StoryVars, TalkSession};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use fmv_mods::Mods;
use std::sync::Arc;

/// A source of the story hook: called once for every conversation.
pub type StoryFactory = Box<dyn Fn() -> Box<dyn StoryVars>>;

/// The story hook for a conversation: the player's own story state as it is when this is called (a copy: what a
/// conversation reads is fixed when it starts), or a new game's when the progression is not FF7's.
pub fn story_of(state: Option<StoryState>) -> StoryFactory {
    let state = state.unwrap_or_else(StoryState::new_game);
    Box::new(move || Box::new(state.clone()))
}

/// Where the original script section of a field comes from (`None`: the field cannot be read).
pub type SectionLoader = Box<dyn Fn(&str) -> Option<Vec<u8>>>;

/// The original sections of this machine's own copy: a field's script section, if the archive has the field and it parses.
fn sections_of(ff7: Arc<Ff7Adapter>) -> SectionLoader {
    Box::new(move |area| {
        let field = ff7.load_field(area).ok()?;
        let script = FieldScript::from_field(field.field_file()).ok()?;
        (script.bytes().len() <= MAX_SECTION_BYTES).then(|| script.bytes().to_vec())
    })
}

/// A script section with the characters its entities declare.
struct Section {
    bytes: Arc<Vec<u8>>,
    characters: Vec<Character>,
}

impl Section {
    fn of(bytes: Arc<Vec<u8>>) -> Option<Self> {
        let characters = characters(&FieldScript::parse(&bytes).ok()?);
        Some(Self { bytes, characters })
    }
}

/// The field script of one area, kept while the player is in it.
struct Loaded {
    key: String,
    /// The original's section; `None` when the area has none that can be read.
    original: Option<Section>,
    /// Echo-S's section for the area, when the text layer is on and the archive has it.
    echo: Option<Section>,
}

/// The conversations of this machine's own FF7 fields.
pub struct Ff7Conversations {
    sections: Option<SectionLoader>,
    area: Option<Loaded>,
    story: StoryFactory,
    /// Counts the conversations: the seed of the next one's `RANDOM`, so that a character who
    /// speaks at random does not say the same thing every time.
    started: u32,
    /// The mod layers (`None`: the base game's conversations, whatever else is configured).
    mods: Option<Arc<Mods>>,
    /// Where voice cues and notes go.
    sink: Arc<dyn ModSink>,
    /// Whether the conversation `start` returned last is a voiced one ([`Conversations::voice`]).
    voiced: bool,
}

/// The biggest script section read (a real one is at most 60 544 bytes).
const MAX_SECTION_BYTES: usize = 1 << 20;

impl Ff7Conversations {
    /// Conversations over `ff7` (`None`: this machine has no FF7 data, nobody has anything to say),
    /// with the story at the start of a new game and the mod layers this process opened ([`mods::active`]).
    pub fn new(ff7: Option<Arc<Ff7Adapter>>) -> Self {
        Self::with_parts(ff7.map(sections_of), mods::active(), Arc::new(mods::Global))
    }

    /// Conversations over `sections` (the original script sections by field), the mod layers `mods` and the sink the voice cues
    /// and notes go to (the tests give their own of each).
    pub fn with_parts(sections: Option<SectionLoader>, mods: Option<Arc<Mods>>, sink: Arc<dyn ModSink>) -> Self {
        Self { sections, area: None, story: Box::new(|| Box::new(StartOfGame)), started: 0, mods, sink, voiced: false }
    }

    /// The script of `area`, loaded now if it is not the one held.
    fn loaded(&mut self, area: &str) -> Option<&Loaded> {
        let sections = self.sections.as_ref()?;
        if self.area.as_ref().is_none_or(|l| l.key != area) {
            let original = sections(area).and_then(|bytes| Section::of(Arc::new(bytes)));
            let echo = self.mods.as_ref().and_then(|m| m.script_section(area)).and_then(Section::of);
            self.area = Some(Loaded { key: area.to_string(), original, echo });
        }
        self.area.as_ref()
    }

    /// The script entity the NPC stands for in the original, with the original section (the server's NPCs are the original's, so a
    /// character the original does not know is nobody).
    fn character(&mut self, area: &str, npc: &NpcView) -> Option<(Arc<Vec<u8>>, Character)> {
        let original = self.loaded(area)?.original.as_ref()?;
        let found = find_character(&original.characters, &npc.name, npc.pos.x, npc.pos.y)?;
        Some((Arc::clone(&original.bytes), found.clone()))
    }

    fn note(&self, line: String) {
        self.sink.note(&line);
    }
}

/// A talk session whose windows send their voice cues: one [`VoiceCue::Say`] for each window that opens and a [`VoiceCue::Stop`]
/// when it is dropped.
struct Voiced {
    inner: TalkSession,
    field: String,
    sink: Arc<dyn ModSink>,
}

impl MessageSource for Voiced {
    fn next(&mut self, answer: Option<usize>) -> Beat {
        let beat = self.inner.next(answer);
        if let Beat::Message(m) = &beat {
            // A window of nothing but codes is skipped by the dialogue window: no voice for what is never shown.
            if let (MessageOrigin::Canon { dialog, .. }, false) = (&m.origin, is_blank(m)) {
                self.sink.cue(VoiceCue::Say { field: self.field.clone(), window: m.window, dialog: *dialog, page: 0 });
            }
        }
        beat
    }
}

impl Drop for Voiced {
    fn drop(&mut self) {
        self.sink.cue(VoiceCue::Stop);
    }
}

impl Conversations for Ff7Conversations {
    fn talk_radius(&mut self, area: &str, npc: &NpcView) -> f32 {
        let Some(loaded) = self.loaded(area) else { return 0.0 };
        let Some(original) = loaded.original.as_ref() else { return 0.0 };
        find_character(&original.characters, &npc.name, npc.pos.x, npc.pos.y).map_or(0.0, |c| c.talk_radius)
    }

    fn start(&mut self, area: &str, npc: &NpcView) -> Option<Box<dyn MessageSource>> {
        self.voiced = false;
        let (section, character) = self.character(area, npc)?;
        self.started = self.started.wrapping_add(1);
        let seed = self.started;
        let layers = self.mods.as_ref().map(|m| m.layers());
        // Echo-S's section first, for a character it has (by name and place) and can talk at this point of the story.
        let echo = self.area.as_ref().and_then(|l| l.echo.as_ref());
        let echo_entity = echo.and_then(|e| find_character(&e.characters, &npc.name, npc.pos.x, npc.pos.y).map(|c| (Arc::clone(&e.bytes), c.entity)));
        if let Some((bytes, entity)) = echo_entity {
            if let Ok(session) = TalkSession::talk(bytes.as_ref().clone(), entity, (self.story)(), seed) {
                self.note(format!("mods: talk {area} {}: echo-s section", npc.name));
                if layers.is_some_and(|l| l.voice) {
                    self.voiced = true;
                    return Some(Box::new(Voiced { inner: session, field: area.to_string(), sink: Arc::clone(&self.sink) }));
                }
                return Some(Box::new(session));
            }
        }
        let session = TalkSession::talk(section.as_ref().clone(), character.entity, (self.story)(), seed).ok()?;
        // Only a run that could have used Echo-S's text says that it did not (the layer off is said once, at start-up).
        if let Some(why) = self.mods.as_ref().filter(|m| m.layers().text && m.has_echo()).map(|_| if echo.is_some() { "echo-s cannot talk there" } else { "not in echo-s" }) {
            self.note(format!("mods: talk {area} {}: original section ({why})", npc.name));
        }
        Some(Box::new(session))
    }

    fn set_story(&mut self, story: StoryFactory) {
        self.story = story;
    }

    fn voice(&mut self) -> Option<Arc<dyn ModSink>> {
        self.voiced.then(|| Arc::clone(&self.sink))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::Pos;
    use fmv_ff7::script::dialogue::{End, Message};
    use fmv_mods::testkit::{echo_archive, script_section, EchoSpec, NPC_AT};
    use fmv_mods::{Layers, ModSources};
    use std::sync::Mutex;

    /// A sink that keeps everything it is given.
    #[derive(Default)]
    struct Collect {
        cues: Mutex<Vec<VoiceCue>>,
        notes: Mutex<Vec<String>>,
    }

    impl ModSink for Collect {
        fn cue(&self, cue: VoiceCue) {
            self.cues.lock().unwrap().push(cue);
        }
        fn note(&self, line: &str) {
            self.notes.lock().unwrap().push(line.to_string());
        }
    }

    impl Collect {
        fn cues(&self) -> Vec<VoiceCue> {
            self.cues.lock().unwrap().clone()
        }
        fn notes(&self) -> Vec<String> {
            self.notes.lock().unwrap().clone()
        }
    }

    fn npc(name: &str) -> NpcView {
        NpcView { id: 0, name: name.into(), model: "aaaa".into(), idle_animation: None, pos: Pos::new(f32::from(NPC_AT.0), f32::from(NPC_AT.1), 0.0), facing: 0.0 }
    }

    /// The text of every window of a conversation, one string per window.
    fn texts(mut source: Box<dyn MessageSource>) -> (Vec<String>, End) {
        let mut out = Vec::new();
        for _ in 0..16 {
            match source.next(None) {
                Beat::Message(m) => out.push(plain(&m)),
                Beat::End(e) => return (out, e),
            }
        }
        panic!("a conversation never ended");
    }

    fn plain(m: &Message) -> String {
        m.page_lines().iter().map(|p| p.join(" ")).collect::<Vec<_>>().join(" / ")
    }

    /// Echo-S's archive on disk (a temp dir) opened with `layers`.
    fn mods_with(spec: &EchoSpec<'_>, layers: Layers) -> (tempfile::TempDir, Arc<Mods>) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echo.iro");
        std::fs::write(&path, echo_archive(spec)).unwrap();
        let m = Mods::open(&ModSources { echo_s: Some(path), ..Default::default() }, layers);
        assert!(m.has_echo(), "{:?}", m.notes());
        (tmp, Arc::new(m))
    }

    fn originals() -> Option<SectionLoader> {
        let section = script_section("npc", &["The first original line", "The second original line"]);
        Some(Box::new(move |area| (area == "md1stin").then(|| section.clone())))
    }

    fn spec() -> EchoSpec<'static> {
        let mut s = EchoSpec::default();
        s.fields.push(("md1stin", vec!["A mod line", "Another mod line", "A third mod line"]));
        s
    }

    #[test]
    fn without_mods_the_original_conversation_plays_and_nothing_is_said() {
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), None, sink.clone());
        let (windows, end) = texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(windows, ["The first original line", "The second original line"]);
        assert_eq!(end, End::Finished);
        assert!(sink.cues().is_empty() && sink.notes().is_empty());
        assert!(c.talk_radius("md1stin", &npc("npc")) > 0.0);
        // An NPC the original does not know, and a field it does not have: nobody to talk to.
        assert!(c.start("md1stin", &npc("stranger")).is_none());
        assert!(c.start("nofield", &npc("npc")).is_none());
        assert_eq!(c.talk_radius("nofield", &npc("npc")), 0.0);
    }

    #[test]
    fn with_the_text_layer_the_character_talks_from_echo_s_own_section() {
        let (_t, mods) = mods_with(&spec(), Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let (windows, _) = texts(c.start("md1stin", &npc("npc")).unwrap());
        // Echo-S's flow: three windows, its own words, not the original's two.
        assert_eq!(windows, ["A mod line", "Another mod line", "A third mod line"]);
        assert_eq!(sink.notes(), ["mods: talk md1stin npc: echo-s section"]);
    }

    #[test]
    fn switching_the_text_layer_off_is_the_original_conversation() {
        let (_t, mods) = mods_with(&spec(), Layers { text: false, ..Layers::ALL });
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let (windows, _) = texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(windows, ["The first original line", "The second original line"]);
        // The layer is off: no Echo-S section, so no voices keyed to it, and no per-conversation note.
        assert!(sink.cues().is_empty() && sink.notes().is_empty(), "{:?} {:?}", sink.cues(), sink.notes());
    }

    #[test]
    fn a_character_echo_s_does_not_have_talks_from_the_original_section() {
        // Echo-S has the field, but its character is another one (named otherwise): the NPC is not found there.
        let mut s = spec();
        s.entity = "someone";
        let (_t, mods) = mods_with(&s, Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let (windows, _) = texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(windows, ["The first original line", "The second original line"]);
        assert_eq!(sink.notes(), ["mods: talk md1stin npc: original section (echo-s cannot talk there)"]);
        assert!(sink.cues().is_empty(), "the original's text has no voices");
        // A field Echo-S has no script for at all says so.
        let mut c = Ff7Conversations::with_parts(
            Some(Box::new({
                let section = script_section("npc", &["Original"]);
                move |area| (area == "elsewhere").then(|| section.clone())
            })),
            Some(mods_with(&spec(), Layers::ALL).1),
            sink.clone(),
        );
        assert_eq!(texts(c.start("elsewhere", &npc("npc")).unwrap()).0, ["Original"]);
        assert_eq!(sink.notes().last().map(String::as_str), Some("mods: talk elsewhere npc: original section (not in echo-s)"));
    }

    #[test]
    fn voices_follow_the_windows_and_stop_when_the_conversation_is_dropped() {
        let (_t, mods) = mods_with(&spec(), Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let mut source = c.start("md1stin", &npc("npc")).unwrap();
        assert!(sink.cues().is_empty(), "nothing is said before a window opens");
        assert!(matches!(source.next(None), Beat::Message(_)));
        assert!(matches!(source.next(None), Beat::Message(_)));
        let say = |dialog| VoiceCue::Say { field: "md1stin".into(), window: 0, dialog, page: 0 };
        assert_eq!(sink.cues(), vec![say(0), say(1)]);
        drop(source);
        assert_eq!(sink.cues(), vec![say(0), say(1), VoiceCue::Stop]);
    }

    #[test]
    fn a_window_that_shows_nothing_has_no_voice_and_the_voice_layer_has_its_own_switch() {
        // The second dialog is a blank (a space): the dialogue window skips it, so no voice either.
        let mut s = EchoSpec::default();
        s.fields.push(("md1stin", vec!["First", " ", "Third"]));
        let (_t, mods) = mods_with(&s, Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let (windows, _) = texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(windows.len(), 3);
        let says: Vec<usize> = sink.cues().iter().filter_map(|c| if let VoiceCue::Say { dialog, .. } = c { Some(*dialog) } else { None }).collect();
        assert_eq!(says, [0, 2]);
        // The voice layer off: Echo-S's text, no cues at all (not even a stop).
        let (_t, mods) = mods_with(&s, Layers { voice: false, ..Layers::ALL });
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let (windows, _) = texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(windows.len(), 3);
        assert!(sink.cues().is_empty());
    }

    #[test]
    fn the_talk_radius_is_always_the_originals() {
        let (_t, mods) = mods_with(&spec(), Layers::ALL);
        let mut with = Ff7Conversations::with_parts(originals(), Some(mods), Arc::new(Collect::default()));
        let mut without = Ff7Conversations::with_parts(originals(), None, Arc::new(Collect::default()));
        assert_eq!(with.talk_radius("md1stin", &npc("npc")), without.talk_radius("md1stin", &npc("npc")));
    }

    #[test]
    fn an_echo_s_character_that_cannot_be_talked_to_falls_back_to_the_original() {
        // Echo-S's version of the character has its talking switched off by its init script (`TLKON 1`): the walker refuses
        // it, the original's is not (it never does that).
        let mut s = spec();
        s.init_extra = vec![0x7E, 1];
        let (_t, mods) = mods_with(&s, Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        let (windows, _) = texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(windows, ["The first original line", "The second original line"]);
        assert_eq!(sink.notes(), ["mods: talk md1stin npc: original section (echo-s cannot talk there)"]);
    }
}
