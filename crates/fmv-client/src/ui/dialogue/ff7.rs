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

use super::echo_live::{self, Skip};
use super::layout::is_blank;
use super::live::{LiveText, LiveVoice};
use super::Conversations;
use crate::live::LiveWindowView;
use crate::mods::{self, ModSink, VoiceCue};
use crate::state::NpcView;
use fmv_ff7::script::dialogue::{characters, find_character, parse_message, Beat, Character, Message, MessageOrigin, MessageSource, StartOfGame, StoryVars, TalkSession};
use fmv_protocol::LiveAsk;
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use fmv_mods::echo::short_voice_name;
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
    /// Echo-S's line for each message instruction of the original, made when the first live window of the area asks.
    live_map: Option<echo_live::LiveMap>,
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
            self.area = Some(Loaded { key: area.to_string(), original, echo, live_map: None });
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

    /// The text of dialog `dialog` of `script`, as pieces under the current story.
    fn pieces_of(&self, script: &FieldScript<'_>, dialog: usize) -> Option<Vec<fmv_ff7::script::dialogue::Piece>> {
        let d = script.dialogs().get(dialog)?;
        let raw = script.bytes().get(d.offset..d.offset.checked_add(d.len)?)?;
        Some(parse_message(raw, &*(self.story)()))
    }

    /// Echo-S's line for the live window `view` of `area` (found by the instruction that opened it, see [`echo_live`]), with the voice
    /// that goes with it; `original` is the original's message of the same window.
    fn echo_window(&mut self, area: &str, mods: &Arc<Mods>, view: &LiveWindowView, original: &Message) -> Result<LiveText, Skip> {
        self.loaded(area).ok_or(Skip::NoSection)?;
        let loaded = self.area.as_mut().ok_or(Skip::NoSection)?;
        let (Some(o), Some(e)) = (loaded.original.as_ref(), loaded.echo.as_ref()) else { return Err(Skip::NoSection) };
        let echo_bytes = Arc::clone(&e.bytes);
        if loaded.live_map.is_none() {
            loaded.live_map = Some(echo_live::build(&o.bytes, &e.bytes));
        }
        let map = loaded.live_map.as_ref().ok_or(Skip::NoSection)?;
        let line = echo_live::lookup(map, view.entity, view.offset, view.dialog)?;
        let script = FieldScript::parse(&echo_bytes).map_err(|_| Skip::NoSection)?;
        let pieces = self.pieces_of(&script, line.dialog).ok_or(Skip::NoText)?;
        let ask = line.ask.map(|(first, last)| LiveAsk { first, last });
        let message = super::live::message_for(line.dialog, line.window, pieces, ask);
        // The server counts the answer from the original question's first line: the same number of lines keeps each choice's place.
        let lines = |m: &Message| m.choice.map(|c| c.last - c.first);
        if lines(&message) != lines(original) {
            return Err(Skip::AskDiffers);
        }
        let voice = mods.layers().voice.then(|| LiveVoice { field: area.to_string(), sink: Arc::clone(&self.sink) });
        if voice.is_some() {
            let heard = mods.voice_path(area, line.window, line.dialog, 0).map_or_else(|| "none".to_string(), |path| short_voice_name(&path));
            self.note(format!("mods: window {area}#{} w{} voice {heard}", line.dialog, line.window));
        }
        Ok(LiveText { message, voice })
    }
}

/// A talk session whose windows send their voice cues: one [`VoiceCue::Say`] for each window that opens and a [`VoiceCue::Stop`]
/// when it is dropped. Each window also says, in one line of ids (never of words), which clip the voice layer has for it:
/// `mods: window md1stin#12 w0 voice md1stin/12a` (or `voice none`), so that a run without ears (a headless one) shows it.
struct Voiced {
    inner: TalkSession,
    field: String,
    sink: Arc<dyn ModSink>,
    mods: Arc<Mods>,
}

impl MessageSource for Voiced {
    fn next(&mut self, answer: Option<usize>) -> Beat {
        let beat = self.inner.next(answer);
        if let Beat::Message(m) = &beat {
            // A window of nothing but codes is skipped by the dialogue window: no voice for what is never shown.
            if let (MessageOrigin::Canon { dialog, .. }, false) = (&m.origin, is_blank(m)) {
                self.sink.cue(VoiceCue::Say { field: self.field.clone(), window: m.window, dialog: *dialog, page: 0 });
                let heard = self.mods.voice_path(&self.field, m.window, *dialog, 0).map_or_else(|| "none".to_string(), |path| short_voice_name(&path));
                self.sink.note(&format!("mods: window {}#{dialog} w{} voice {heard}", self.field, m.window));
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
        // Echo-S's section first, for a character it has (by name and place) and can talk at this point of the story.
        let echo = self.area.as_ref().and_then(|l| l.echo.as_ref());
        let echo_entity = echo.and_then(|e| find_character(&e.characters, &npc.name, npc.pos.x, npc.pos.y).map(|c| (Arc::clone(&e.bytes), c.entity)));
        if let Some((bytes, entity)) = echo_entity {
            if let Ok(session) = TalkSession::talk(bytes.as_ref().clone(), entity, (self.story)(), seed) {
                self.note(format!("mods: talk {area} {}: echo-s section", npc.name));
                if let Some(mods) = self.mods.as_ref().filter(|m| m.layers().voice) {
                    self.voiced = true;
                    return Some(Box::new(Voiced { inner: session, field: area.to_string(), sink: Arc::clone(&self.sink), mods: Arc::clone(mods) }));
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

    fn live_message(&mut self, area: &str, window: u8, dialog: usize, ask: Option<LiveAsk>) -> Option<Message> {
        // The ORIGINAL's table, whatever the text layer: the server runs the original's scripts and names its dialogs.
        let bytes = Arc::clone(&self.loaded(area)?.original.as_ref()?.bytes);
        let script = FieldScript::parse(&bytes).ok()?;
        let pieces = self.pieces_of(&script, dialog)?;
        Some(super::live::message_for(dialog, window, pieces, ask))
    }

    fn live_window(&mut self, area: &str, view: &LiveWindowView) -> Option<LiveText> {
        let original = self.live_message(area, view.window, usize::from(view.dialog), view.ask)?;
        // The text layer: Echo-S's line (and voice) for the instruction that opened the window, else the original's text.
        let Some(mods) = self.mods.clone().filter(|m| m.layers().text && m.has_echo()) else { return Some(LiveText { message: original, voice: None }) };
        match self.echo_window(area, &mods, view, &original) {
            Ok(text) => Some(text),
            Err(skip) => {
                self.note(format!("mods: live {area} e{}@{} #{}: original text ({})", view.entity, view.offset, view.dialog, skip.why()));
                Some(LiveText { message: original, voice: None })
            }
        }
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
        assert_eq!(sink.notes().first().map(String::as_str), Some("mods: talk md1stin npc: echo-s section"));
    }

    #[test]
    fn each_window_says_which_voice_it_looked_up_by_ids_only() {
        let mut s = spec();
        s.voices.push("VO\\Voice\\md1stin\\0a.ogg");
        s.voices.push("VO\\Voice\\md1stin\\2.ogg");
        let (_t, mods) = mods_with(&s, Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        texts(c.start("md1stin", &npc("npc")).unwrap());
        assert_eq!(
            sink.notes(),
            [
                "mods: talk md1stin npc: echo-s section",
                "mods: window md1stin#0 w0 voice md1stin/0a",
                "mods: window md1stin#1 w0 voice none",
                "mods: window md1stin#2 w0 voice md1stin/2",
            ]
        );
        // With the voice layer off no window says anything (no cues, no lookups).
        let (_t, mods) = mods_with(&s, Layers { voice: false, ..Layers::ALL });
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(originals(), Some(mods), sink.clone());
        texts(c.start("md1stin", &npc("npc")).unwrap());
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

    /// The real conversations of the first reactor fields on the owner's data, through the client's own conversation code: which
    /// characters talk from Echo-S's section and which window finds which voice clip. Read-only; only counts and ids are printed.
    #[test]
    #[ignore = "needs the owner's FF7 data and Echo-S (FMV_FF7_DATA, FMV_ECHO_S_IRO)"]
    fn the_real_conversations_of_the_reactor_fields_run_on_echo_s_and_find_their_voices() {
        use fmv_ff7::field::FieldFile;
        use fmv_ff7::lgp::LgpArchive;
        let var = |name: &str| std::env::var_os(name).map(std::path::PathBuf::from);
        let (Some(data), Some(echo)) = (var("FMV_FF7_DATA"), var("FMV_ECHO_S_IRO")) else {
            eprintln!("FMV_FF7_DATA and FMV_ECHO_S_IRO are needed; skipping");
            return;
        };
        let lgp = Arc::new(Mutex::new(LgpArchive::open_path(data.join("field").join("flevel.lgp")).expect("flevel.lgp")));
        let section_of = move |area: &str| -> Option<Vec<u8>> {
            let stored = lgp.lock().unwrap().read(area).ok()?;
            let file = FieldFile::parse(fmv_ff7::lzss::decompress(&stored).ok()?).ok()?;
            Some(FieldScript::from_field(&file).ok()?.bytes().to_vec())
        };
        let mods = Arc::new(Mods::open(&ModSources { echo_s: Some(echo), ..Default::default() }, Layers::ALL));
        assert!(mods.notes().is_empty(), "{:?}", mods.notes());
        let sink = Arc::new(Collect::default());
        let loader = {
            let section_of = section_of.clone();
            Box::new(move |area: &str| section_of(area)) as SectionLoader
        };
        let mut conversations = Ff7Conversations::with_parts(Some(loader), Some(Arc::clone(&mods)), sink.clone());
        let (mut npcs, mut from_echo, mut from_original, mut windows) = (0usize, 0usize, 0usize, 0usize);
        for field in ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"] {
            let Some(bytes) = section_of(field) else { continue };
            let script = FieldScript::parse(&bytes).expect("script");
            for c in characters(&script) {
                npcs += 1;
                let place = c.places.first().map_or((0.0, 0.0), |&(x, y)| (f32::from(x), f32::from(y)));
                let view = NpcView { id: 0, name: c.name.clone(), model: String::new(), idle_animation: None, pos: Pos::new(place.0, place.1, 0.0), facing: 0.0 };
                let before = sink.notes().len();
                let Some(source) = conversations.start(field, &view) else { continue };
                let (shown, _) = texts(source);
                windows += shown.len();
                let said = sink.notes();
                match said.get(before).map(String::as_str) {
                    Some(l) if l.contains("echo-s section") => from_echo += 1,
                    _ => from_original += 1,
                }
            }
        }
        let notes = sink.notes();
        let voiced = notes.iter().filter(|l| l.starts_with("mods: window") && !l.ends_with("voice none")).count();
        let silent = notes.iter().filter(|l| l.starts_with("mods: window") && l.ends_with("voice none")).count();
        for line in notes.iter().filter(|l| l.starts_with("mods: window")).take(24) {
            println!("{line}");
        }
        println!("reactor fields: {npcs} characters; {from_echo} talk from Echo-S's section, {from_original} from the original's; {windows} windows shown; windows of Echo-S's text: {voiced} voiced, {silent} without a clip");
        assert!(from_echo > 0 && voiced > 0, "some real conversation is voiced");
    }

    // ---- live windows ------------------------------------------------------------------------------

    use super::super::echo_live::tests::{ask, code, msg, offset_of, section};
    use super::super::Talk;
    use crate::live::LiveClient;
    use fmv_protocol::ClientMsg;
    use winit::keyboard::KeyCode;

    /// An original with a message then a question (the question's lines are 1-2), and an Echo-S section for the same entity with
    /// its dialogs numbered otherwise and the question's lines at 2-3.
    fn live_sections() -> (Vec<u8>, Vec<u8>) {
        let original = section(&[("a", vec![(1, code(&[msg(0, 0), ask(1, 1, 1, 2)]))])], &["Orig hello", "Orig go?\n\tOrig yes\n\tOrig no"]);
        let echo = section(
            &[("a", vec![(1, code(&[msg(2, 3), ask(3, 0, 2, 3)]))])],
            &["Mod go?\nextra\n\tMod yes\n\tMod no", "u", "v", "Mod hello"],
        );
        (original, echo)
    }

    fn live_mods(echo: &[u8], voices: &[&'static str], layers: Layers) -> (tempfile::TempDir, Arc<Mods>) {
        let mut s = EchoSpec::default();
        s.blobs.push(("Echo-S\\flevel.lgp\\md1stin", fmv_mods::testkit::field_entry(echo)));
        s.voices.extend_from_slice(voices);
        mods_with(&s, layers)
    }

    fn live_loader(original: Vec<u8>) -> Option<SectionLoader> {
        Some(Box::new(move |area| (area == "md1stin").then(|| original.clone())))
    }

    fn view(original: &[u8], k: usize, dialog: u16, ask: Option<LiveAsk>) -> LiveWindowView {
        LiveWindowView::new(0, dialog, ask, true).opened_by(0, offset_of(original, 0, 1, k))
    }

    #[test]
    fn a_live_message_shows_echo_s_line_found_by_its_instruction_and_cues_its_voice() {
        let (original, echo) = live_sections();
        let (_t, mods) = live_mods(&echo, &["VO\\Voice\\md1stin\\w2_3.ogg"], Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(live_loader(original.clone()), Some(mods), sink.clone());
        let text = c.live_window("md1stin", &view(&original, 0, 0, None)).unwrap();
        assert_eq!(plain(&text.message), "Mod hello");
        // Echo-S's own numbers: its dialog and the window its instruction names (what its voice files are keyed by).
        assert!(matches!(text.message.origin, MessageOrigin::Canon { dialog: 3, .. }));
        assert_eq!(text.message.window, 2);
        assert_eq!(text.voice.as_ref().map(|v| v.field.as_str()), Some("md1stin"));
        assert_eq!(sink.notes(), ["mods: window md1stin#3 w2 voice md1stin/w2_3"]);
    }

    #[test]
    fn a_live_question_keeps_the_original_s_answer_positions_over_echo_s_lines() {
        let (original, echo) = live_sections();
        let (_t, mods) = live_mods(&echo, &[], Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(live_loader(original.clone()), Some(mods), sink.clone());
        let asked = Some(LiveAsk { first: 1, last: 2 });
        let text = c.live_window("md1stin", &view(&original, 1, 1, asked)).unwrap();
        assert!(matches!(text.message.origin, MessageOrigin::Canon { dialog: 0, .. }));
        // Echo-S's question has its lines at 2-3, the original's at 1-2.
        assert_eq!(text.message.choice.map(|c| (c.first, c.last)), Some((2, 3)));
        // Through the window: pick the second line; the server hears position 1 (counted from the original's first line).
        let mut talk = Talk::new(Box::new(Ff7Conversations::with_parts(live_loader(original.clone()), Some(live_mods(&echo, &[], Layers::ALL).1), sink.clone())));
        let mut live = LiveClient::default();
        live.on_window(view(&original, 1, 1, asked));
        talk.sync_live(&mut live, "md1stin", || super::story_of(None));
        assert!(talk.live_is_open());
        assert!(talk.live_key(&mut live, KeyCode::ArrowDown, true, false));
        assert!(talk.live_key(&mut live, KeyCode::Enter, true, false));
        assert_eq!(live.take_outbox(), vec![ClientMsg::LiveAnswer { window: 0, choice: Some(1) }]);
    }

    #[test]
    fn a_live_window_cues_its_voice_pages_and_choices_and_stops_when_it_goes() {
        let (original, echo) = live_sections();
        let (_t, mods) = live_mods(&echo, &["VO\\Voice\\md1stin\\0.ogg"], Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut talk = Talk::new(Box::new(Ff7Conversations::with_parts(live_loader(original.clone()), Some(mods), sink.clone())));
        let mut live = LiveClient::default();
        live.on_window(view(&original, 1, 1, Some(LiveAsk { first: 1, last: 2 })));
        talk.sync_live(&mut live, "md1stin", || super::story_of(None));
        let say = VoiceCue::Say { field: "md1stin".into(), window: 3, dialog: 0, page: 0 };
        assert_eq!(sink.cues(), vec![say.clone()]);
        assert!(talk.live_key(&mut live, KeyCode::ArrowDown, true, false));
        assert_eq!(sink.cues().last(), Some(&VoiceCue::Choice { field: "md1stin".into(), dialog: 0, line: 3 }));
        // The server closes the window: the voice stops.
        live.on_window_closed(0);
        talk.sync_live(&mut live, "md1stin", || super::story_of(None));
        assert_eq!(sink.cues().last(), Some(&VoiceCue::Stop));
        assert!(!talk.live_is_open());
    }

    #[test]
    fn a_live_window_without_a_counterpart_shows_the_original_text_and_says_why_in_ids() {
        let (original, echo) = live_sections();
        let (_t, mods) = live_mods(&echo, &[], Layers::ALL);
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(live_loader(original.clone()), Some(mods), sink.clone());
        // An instruction the original does not have there, and a dialog the instruction does not open.
        let mut v = view(&original, 0, 0, None);
        v.offset += 1;
        assert_eq!(plain(&c.live_window("md1stin", &v).unwrap().message), "Orig hello");
        assert_eq!(plain(&c.live_window("md1stin", &view(&original, 0, 1, None)).unwrap().message), "Orig go? Orig yes Orig no");
        // A question whose lines Echo-S does not have as many of.
        let few = section(&[("a", vec![(1, code(&[msg(2, 3), ask(3, 0, 2, 2)]))])], &["Mod go?\nx\n\tMod yes", "u", "v", "Mod hello"]);
        let (_t2, mods) = live_mods(&few, &[], Layers::ALL);
        let mut c = Ff7Conversations::with_parts(live_loader(original.clone()), Some(mods), sink.clone());
        let asked = Some(LiveAsk { first: 1, last: 2 });
        assert_eq!(plain(&c.live_window("md1stin", &view(&original, 1, 1, asked)).unwrap().message), "Orig go? Orig yes Orig no");
        assert_eq!(
            sink.notes(),
            [
                format!("mods: live md1stin e0@{} #0: original text (no such instruction)", v.offset),
                format!("mods: live md1stin e0@{} #1: original text (dialog differs)", view(&original, 0, 0, None).offset),
                format!("mods: live md1stin e0@{} #1: original text (question differs)", view(&original, 1, 1, None).offset),
            ]
        );
        assert!(sink.cues().is_empty());
    }

    #[test]
    fn the_live_text_is_the_originals_with_the_text_layer_off_or_no_echo_section() {
        let (original, echo) = live_sections();
        let (_t, mods) = live_mods(&echo, &[], Layers { text: false, ..Layers::ALL });
        let sink = Arc::new(Collect::default());
        let mut c = Ff7Conversations::with_parts(live_loader(original.clone()), Some(mods), sink.clone());
        assert_eq!(plain(&c.live_window("md1stin", &view(&original, 0, 0, None)).unwrap().message), "Orig hello");
        let mut c = Ff7Conversations::with_parts(live_loader(original.clone()), None, sink.clone());
        assert_eq!(plain(&c.live_window("md1stin", &view(&original, 0, 0, None)).unwrap().message), "Orig hello");
        assert!(sink.notes().is_empty() && sink.cues().is_empty(), "{:?}", sink.notes());
        // The layer on, but Echo-S has no script for this area: the original's text, with a note.
        let (_t, mods) = live_mods(&echo, &[], Layers::ALL);
        let mut c = Ff7Conversations::with_parts(Some(Box::new({ let o = original.clone(); move |_| Some(o.clone()) })), Some(mods), sink.clone());
        assert_eq!(plain(&c.live_window("elsewhere", &view(&original, 0, 0, None)).unwrap().message), "Orig hello");
        assert_eq!(sink.notes().len(), 1);
        assert!(sink.notes()[0].ends_with("original text (not in echo-s)"), "{:?}", sink.notes());
    }

    /// How many real message instructions of the first reactor fields (every `MESSAGE`/`ASK` of the original, as a live window the
    /// server would open) get Echo-S's line through the client's own live path, and why the others do not. Static: not the windows a
    /// run reaches. Read-only; only counts are printed.
    #[test]
    #[ignore = "needs the owner's FF7 data and Echo-S (FMV_FF7_DATA, FMV_ECHO_S_IRO)"]
    fn the_real_live_windows_of_the_reactor_fields_find_echo_s_lines() {
        use fmv_ff7::field::FieldFile;
        use fmv_ff7::lgp::LgpArchive;
        use std::collections::{BTreeMap, BTreeSet};
        let var = |name: &str| std::env::var_os(name).map(std::path::PathBuf::from);
        let (Some(data), Some(echo)) = (var("FMV_FF7_DATA"), var("FMV_ECHO_S_IRO")) else {
            eprintln!("FMV_FF7_DATA and FMV_ECHO_S_IRO are needed; skipping");
            return;
        };
        let lgp = Arc::new(Mutex::new(LgpArchive::open_path(data.join("field").join("flevel.lgp")).expect("flevel.lgp")));
        let section_of = move |area: &str| -> Option<Vec<u8>> {
            let stored = lgp.lock().unwrap().read(area).ok()?;
            let file = FieldFile::parse(fmv_ff7::lzss::decompress(&stored).ok()?).ok()?;
            Some(FieldScript::from_field(&file).ok()?.bytes().to_vec())
        };
        let mods = Arc::new(Mods::open(&ModSources { echo_s: Some(echo), ..Default::default() }, Layers::ALL));
        let sink = Arc::new(Collect::default());
        let loader = {
            let section_of = section_of.clone();
            Box::new(move |area: &str| section_of(area)) as SectionLoader
        };
        let mut conversations = Ff7Conversations::with_parts(Some(loader), Some(mods), sink.clone());
        let (mut echo_lines, mut asks, mut echo_asks) = (0usize, 0usize, 0usize);
        let mut fallbacks: BTreeMap<String, usize> = BTreeMap::new();
        for field in ["md1stin", "md1_1", "md1_2"] {
            let Some(bytes) = section_of(field) else { continue };
            let script = FieldScript::parse(&bytes).expect("script");
            let mut seen = BTreeSet::new();
            let mut count = 0usize;
            for entity in 0..script.entity_count() {
                for slot in 0..32 {
                    let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
                    for i in d.instructions.iter().filter(|i| matches!(i.opcode, 0x40 | 0x48)) {
                        if !seen.insert((entity, i.offset)) {
                            continue;
                        }
                        count += 1;
                        let lit = |n: &str| i.literal(n).and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
                        let ask = (i.opcode == 0x48).then(|| LiveAsk { first: lit("first"), last: lit("last") });
                        let view = LiveWindowView::new(lit("window"), u16::from(lit("dialog")), ask, true).opened_by(u16::try_from(entity).unwrap(), u32::try_from(i.offset).unwrap());
                        let before = sink.notes().len();
                        if conversations.live_window(field, &view).is_none() {
                            *fallbacks.entry("no original text".into()).or_default() += 1;
                            continue;
                        }
                        asks += usize::from(ask.is_some());
                        let notes = sink.notes();
                        match notes.get(before..).and_then(|n| n.iter().find(|l| l.starts_with("mods: live"))) {
                            None => {
                                echo_lines += 1;
                                echo_asks += usize::from(ask.is_some());
                            }
                            Some(l) => *fallbacks.entry(l.rsplit('(').next().unwrap_or("").trim_end_matches(')').to_string()).or_default() += 1,
                        }
                    }
                }
            }
            println!("{field}: {count} message instructions");
        }
        println!("message instructions: {echo_lines} get Echo-S's line ({echo_asks} of {asks} questions), the others the original's: {fallbacks:?}");
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
