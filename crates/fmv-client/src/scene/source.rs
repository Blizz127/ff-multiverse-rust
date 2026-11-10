//! Where a scene comes from on this machine: the seam the scene player reads through, and the builder that makes one from
//! this machine's own copy of the game.
//!
//! * [`SceneSource`] is the seam: what the player needs of a scene, as the interpreter gives it (the places the cast stands
//!   at when it begins, then a beat per window, the ground's height). `fmv_ff7::script::scene::ScenePlay` is the real one;
//!   the tests give a scripted fake, so the state machine is tested with no game data and no GPU.
//! * [`SceneBuilder`] makes a source (and the scene's data) from a scene id, the story it plays against and the seed the
//!   server chose, and says how far a player has to stand from a scene's trigger. [`Ff7Scenes`] reads this machine's field;
//!   [`NoScenes`] is what a client without game data has: it can build nothing, which is a scene that cannot be played here
//!   (the player then releases itself, see [`super::SceneClient`]).
//!
//! Nothing here carries game text or bytes out of the process: a failure is described in ids and error kinds only.

use fmv_ff7::script::scene::{scene, scenes, trigger_npc, Beat, SceneDef, ScenePlay, Stage};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use std::collections::HashMap;
use std::sync::Arc;

/// What the scene player reads a scene through.
pub trait SceneSource {
    /// Where the cast stands when the scene begins.
    fn initial(&self) -> Vec<Stage>;

    /// The next beat: the stage events up to the next window and that window (or the end). `answer` is the answer to the
    /// window handed out last when it asked (the absolute line number the player chose).
    fn next(&mut self, answer: Option<usize>) -> Beat;

    /// Translate a displayed answer into the original server script's answer.
    fn server_answer(&self, _window: &fmv_ff7::script::scene::Window, answer: Option<usize>) -> Result<Option<usize>, ()> { Ok(answer) }
    /// A bot's answer is expressed in original-script line numbers.
    fn bot_answer(&self, _window: &fmv_ff7::script::scene::Window, answer: Option<usize>) -> Option<usize> { answer }
    /// Whether this source uses Echo-S's voice-numbered messages.
    fn echo_voice(&self) -> bool { false }

    /// The ground's height at a place of the field (the triangle the scene says holds it, if it does): where the witness
    /// stands, and any actor the scene places without a height of its own. `None`: not known here.
    fn height_at(&self, _x: i16, _y: i16, _triangle: Option<u16>) -> Option<f32> {
        None
    }
}

impl SceneSource for ScenePlay {
    fn initial(&self) -> Vec<Stage> {
        self.initial_stage_events().to_vec()
    }

    fn next(&mut self, answer: Option<usize>) -> Beat {
        ScenePlay::next(self, answer)
    }

    fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> {
        self.ground().height(x, y, triangle, None)
    }
}

/// The measured Echo-S 1.24 bar script section; a different section gets no guessed speakers.
const ECHO_BAR_SECTION: &str = "6b0735a74596a337761e711c1bd048b5073c3ee53d1523bcf4f2fa9e3911329e";

fn known_echo_bar(section: &[u8]) -> bool { blake3::hash(section).to_hex().as_str() == ECHO_BAR_SECTION }

fn echo_bar_speaker(dialog: usize) -> Option<&'static str> {
    match dialog {
        30..=35 | 37 | 38 | 40 | 42 => Some("tifa"),
        36 | 39 | 41 => Some("cloud"),
        _ => None,
    }
}

struct PrivateEcho { play: ScenePlay, known_bar: bool }

impl SceneSource for PrivateEcho {
    fn initial(&self) -> Vec<Stage> { self.play.initial_stage_events().to_vec() }
    fn next(&mut self, answer: Option<usize>) -> Beat {
        let mut beat = self.play.next(answer);
        if self.known_bar {
            if let fmv_ff7::script::scene::Then::Window(w) = &mut beat.then {
                if w.speaker.is_none() {
                    if let fmv_ff7::script::dialogue::MessageOrigin::Canon { dialog, .. } = &w.message.origin {
                        if let Some(name) = echo_bar_speaker(*dialog) {
                            w.speaker = self.play.def().cast.iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| m.entity);
                        }
                    }
                }
            }
        }
        beat
    }
    fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.play.ground().height(x, y, triangle, None) }
    fn echo_voice(&self) -> bool { true }
    fn bot_answer(&self, window: &fmv_ff7::script::scene::Window, answer: Option<usize>) -> Option<usize> {
        if window.message.choice.is_some() { answer.map(|line| line.saturating_sub(1)) } else { answer }
    }
    fn server_answer(&self, window: &fmv_ff7::script::scene::Window, answer: Option<usize>) -> Result<Option<usize>, ()> {
        match (&window.message.choice, answer) {
            (None, None) => Ok(None),
            (Some(c), Some(a)) if (c.first, c.last) == (0, 1) && a <= 1 => Ok(Some(a + 1)),
            _ => Err(()),
        }
    }
}

/// Validate the one measured bar choice on each layer against the server's original script.
/// This is deliberately scene-specific: differing window counts cannot drive watchers in lockstep.
fn compatible_bar(field: &fmv_ff7::field::FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8]) -> bool {
    fn result(mut play: ScenePlay, answer: usize, range: (usize, usize)) -> Option<(fmv_ff7::script::scene::Writes, End)> {
        let setup = play.setup_stats();
        if !setup.unknown.is_empty() || setup.unresolved != 0 || setup.dropped_requests != 0 { return None; }
        let mut next = None;
        let mut questions = 0;
        for _ in 0..=64 {
            match play.next(next.take()).then {
                fmv_ff7::script::scene::Then::End { how, .. } => {
                    let harmless = ["MENU2", "TALKR", "UC", "WREST", "SOUND"];
                    if how != End::Finished || questions != 1 || !play.stats().unknown.is_empty() || play.stats().unresolved != 0 || play.stats().dropped_requests != 0
                        || play.stats().ignored.keys().any(|op| !harmless.contains(op)) { return None; }
                    return Some((play.writes(), how));
                }
                fmv_ff7::script::scene::Then::Window(w) => if let Some(c) = w.message.choice {
                    if (c.first, c.last) != range { return None; }
                    questions += 1;
                    next = Some(answer);
                },
            }
        }
        None
    }
    for n in 0..2 {
        let (Ok(original), Ok(echo)) = (ScenePlay::new(field, def, story, seed), ScenePlay::with_script_section(field, def, story, seed, section)) else { return false };
        let (Some((a, _)), Some((b, _))) = (result(original, n + 1, (1, 2)), result(echo, n, (0, 1))) else { return false };
        let persistent = |w: &fmv_ff7::script::scene::Writes| w.untracked.iter().filter(|((bank, _), _)| (1..=4).contains(bank)).map(|(k,v)| (*k,*v)).collect::<std::collections::BTreeMap<_,_>>();
        if a.moment != b.moment || a.tracked != b.tracked || persistent(&a) != persistent(&b) { return false; }
    }
    true
}

/// Whether a scene that asks nothing may be shown from the replacement section `section`: run to its end in both editions
/// (bounded, like every run), neither asks a question, and both come to the same story
/// (`Outcome::same_story_as`: the same end and destination, the same saved writes, the same party changes, the same skipped
/// instructions that touch the player's state). The windows may differ in number and wording, which is the point; that is
/// why this is only ever used for a scene nobody watches (a watcher is kept in step window by window).
fn compatible_without_questions(field: &fmv_ff7::field::FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8]) -> bool {
    use fmv_ff7::script::scene::{run_section_to_end, run_to_end};
    let (Ok(original), Ok(echo)) = (run_to_end(field, def, story, seed, &[]), run_section_to_end(field, def, story, seed, section, &[])) else { return false };
    // (a cutscene of a story point that starts a battle first ends at that battle, which the server then starts)
    let ended = |how: End| matches!(how, End::Finished | End::MapJump { .. }) || (how == End::Battle && def.presents.is_some());
    original.questions == 0 && echo.questions == 0 && ended(original.end) && echo.windows > 0 && original.same_story_as(&echo)
}

/// A scene made ready to play: its data and its source.
pub struct Built {
    pub def: SceneDef,
    pub source: Box<dyn SceneSource>,
}

/// Makes the scenes this machine can play.
pub trait SceneBuilder {
    /// The scene `scene` against `story` with `seed`. `Err` says why it cannot be played here (ids and error kinds only).
    fn build(&mut self, scene: &str, story: &StoryState, seed: u32) -> Result<Built, String>;

    /// Private scenes have no watcher whose windows need to match this client's text layer.
    fn build_private(&mut self, scene: &str, story: &StoryState, seed: u32) -> Result<Built, String> { self.build(scene, story, seed) }

    /// How near a player has to stand to the trigger of the scene whose index in the game's list is `index` (walkmesh
    /// units), in the field `area`; `None` when this machine cannot tell (no game data, or the scene is not in that field).
    /// It does not depend on the story the player has: it is the scene's own number at the field's scale.
    fn reach(&mut self, index: usize, area: &str) -> Option<f32>;
}

/// A client with no game data: nothing can be built, and no trigger's reach is known.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoScenes;

impl SceneBuilder for NoScenes {
    fn build(&mut self, _scene: &str, _story: &StoryState, _seed: u32) -> Result<Built, String> {
        Err("this client has no game data to build scenes from".to_string())
    }

    fn reach(&mut self, _index: usize, _area: &str) -> Option<f32> {
        None
    }
}

/// FF7's scenes, built from this machine's own field archive (read-only).
pub struct Ff7Scenes {
    adapter: Option<Arc<Ff7Adapter>>,
    /// The reach of each trigger that could be told, by `(field, scene index)`: the answer needs the field's script, which
    /// is slow to read, and it does not change with the story. (An answer of `None` is not kept: it is asked again.)
    reach: HashMap<(String, usize), f32>,
}

impl Ff7Scenes {
    /// Scenes over `adapter` (`None`: this machine has no FF7 data: [`NoScenes`]'s answers).
    pub fn new(adapter: Option<Arc<Ff7Adapter>>) -> Self {
        Self { adapter, reach: HashMap::new() }
    }
}

impl SceneBuilder for Ff7Scenes {
    fn build(&mut self, id: &str, story: &StoryState, seed: u32) -> Result<Built, String> {
        let def = scene(id).ok_or_else(|| "the game has no scene of that id".to_string())?;
        let adapter = self.adapter.as_ref().ok_or_else(|| "this machine has no FF7 data to build scenes from".to_string())?;
        let area = adapter.load_field(&def.field).map_err(|e| format!("the scene's field cannot be loaded here: {e}"))?;
        let play = ScenePlay::new(area.field_file(), &def, story, seed).map_err(|e| format!("the scene cannot be set up here: {e}"))?;
        Ok(Built { def, source: Box::new(play) })
    }

    fn build_private(&mut self, id: &str, story: &StoryState, seed: u32) -> Result<Built, String> {
        let fallback = self.build(id, story, seed)?;
        let Some(source) = crate::mods::script_source() else { return Ok(fallback) };
        let Some(section) = source.section(&fallback.def.field) else { return Ok(fallback) };
        let Some(adapter) = self.adapter.as_ref() else { return Ok(fallback) };
        let Ok(field) = adapter.load_field(&fallback.def.field) else { return Ok(fallback) };
        // The bar scene asks a question, so it has its own measured check (the answers are translated). Any other scene may
        // use the replacement only if it asks nothing in either edition and both come to the same story.
        let is_bar = id == fmv_ff7::script::scene::PROOF_SCENE_ID;
        let compatible = if is_bar { compatible_bar(field.field_file(), &fallback.def, story, seed, &section) }
            else { compatible_without_questions(field.field_file(), &fallback.def, story, seed, &section) };
        if !compatible { return Ok(fallback); }
        let Ok(play) = ScenePlay::with_script_section(field.field_file(), &fallback.def, story, seed, &section) else { return Ok(fallback) };
        let known_bar = is_bar && known_echo_bar(&section);
        Ok(Built { def: play.def().clone(), source: Box::new(PrivateEcho { play, known_bar }) })
    }

    fn reach(&mut self, index: usize, area: &str) -> Option<f32> {
        let key = (area.to_ascii_lowercase(), index);
        if let Some(known) = self.reach.get(&key) {
            return Some(*known);
        }
        let found = self.find_reach(index, area)?;
        self.reach.insert(key, found);
        Some(found)
    }
}

impl Ff7Scenes {
    /// The trigger's talk radius, read with the scene's own story (its first moment): the stage scripts that stand the
    /// trigger up run for the moment the scene is offered at, whatever the player's own story is, and the radius is the
    /// script's number at the field's scale.
    fn find_reach(&self, index: usize, area: &str) -> Option<f32> {
        let def = scenes().into_iter().nth(index)?;
        if !def.field.eq_ignore_ascii_case(area) {
            return None;
        }
        let story = StoryState::at(*def.trigger.moments.start());
        let field = self.adapter.as_ref()?.load_field(&def.field).ok()?;
        trigger_npc(field.field_file(), &def, &story).map(|t| t.talk_radius).filter(|r| r.is_finite() && *r > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_without_data_builds_nothing_and_knows_no_reach() {
        assert!(!known_echo_bar(b"another script"));
        assert_eq!(echo_bar_speaker(36), Some("cloud"));
        assert_eq!(echo_bar_speaker(35), Some("tifa"));
        assert_eq!(echo_bar_speaker(43), None);
        let mut none = NoScenes;
        assert!(none.build("ff7/mds7pb_1/tifa-bar", &StoryState::at(72), 1).is_err());
        assert_eq!(none.reach(0, "mds7pb_1"), None);
        let mut ff7 = Ff7Scenes::new(None);
        assert!(ff7.build("ff7/mds7pb_1/tifa-bar", &StoryState::at(72), 1).is_err(), "no adapter, no scene");
        assert!(ff7.build("ff7/nowhere/nothing", &StoryState::at(72), 1).is_err(), "no such scene");
        assert_eq!(ff7.reach(0, "mds7pb_1"), None);
        assert_eq!(ff7.reach(0, "md1stin"), None, "the proof scene is not in that field");
        assert_eq!(ff7.reach(99, "mds7pb_1"), None, "and there is no scene 99");
    }

    /// The two scenes the escape ends in ask nothing, and Echo-S's edition of each comes to the same story as the original
    /// (the same end and destination, the same moment, the same party change), so a player alone in one is shown Echo-S's
    /// windows, with a voice where the archive has one; with the text layer off, the original.
    #[test]
    #[ignore = "requires the owner's FF7 data and verified Echo-S archive"]
    fn private_post_escape_scenes_use_echo_when_both_editions_tell_the_same_story() {
        use fmv_core::{EditionProvenance, GameId, Platform};
        use fmv_ff7::script::scene::{run_section_to_end, run_to_end, POST_ESCAPE_SCENE_ID, STATION_SCENE_ID};
        use fmv_mods::{Layers, ModSources, Mods};
        let data = std::env::var_os("FMV_FF7_DATA").expect("FMV_FF7_DATA");
        let echo = std::env::var_os("FMV_ECHO_S_IRO").expect("FMV_ECHO_S_IRO");
        let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
        let adapter = Arc::new(Ff7Adapter::from_flevel_path(std::path::Path::new(&data).join("field/flevel.lgp"), provenance).unwrap());
        let sources = ModSources { echo_s: Some(echo.into()), ..Default::default() };
        let mods = Arc::new(Mods::open(&sources, Layers::ALL));
        crate::mods::install(Some(mods.clone()));
        let story = StoryState::at(33);
        for id in [POST_ESCAPE_SCENE_ID, STATION_SCENE_ID] {
            let def = scene(id).expect("an offered scene");
            let field = adapter.load_field(&def.field).unwrap();
            let section = crate::mods::script_source().unwrap().section(&def.field).expect("Echo-S has the field");
            let original = run_to_end(field.field_file(), &def, &story, 1, &[]).unwrap();
            let replacement = run_section_to_end(field.field_file(), &def, &story, 1, &section, &[]).unwrap();
            eprintln!(
                "{id}: original windows={} end={:?} party={:?} writes={:?}; echo windows={} end={:?} party={:?} writes={:?} echo ignored={:?}",
                original.windows, original.end, original.party, original.writes, replacement.windows, replacement.end, replacement.party, replacement.writes, replacement.stats.ignored
            );
            assert!(original.same_story_as(&replacement), "{id}: the two editions tell the same story");
            let mut builder = Ff7Scenes::new(Some(adapter.clone()));
            let mut built = builder.build_private(id, &story, 1).unwrap();
            assert!(built.source.echo_voice(), "{id}: alone, the player is shown Echo-S");
            let (mut count, mut voices) = (0usize, 0usize);
            loop {
                match built.source.next(None).then {
                    fmv_ff7::script::scene::Then::End { how, .. } => {
                        assert_eq!(how, original.end, "{id}");
                        break;
                    }
                    fmv_ff7::script::scene::Then::Window(w) => {
                        count += 1;
                        assert!(w.message.choice.is_none(), "{id}: asks nothing");
                        assert_eq!(built.source.server_answer(&w, None), Ok(None));
                        if let fmv_ff7::script::dialogue::MessageOrigin::Canon { dialog, .. } = &w.message.origin {
                            voices += usize::from(mods.voice_path(&def.field, w.id, *dialog, 0).is_some());
                        }
                    }
                }
                assert!(count <= 64);
            }
            assert_eq!(count, replacement.windows, "{id}");
            eprintln!("{id}: {count} Echo-S windows, {voices} with a voice clip");
            assert!(!builder.build(id, &story, 1).unwrap().source.echo_voice(), "{id}: a shared run keeps the original");
        }
        crate::mods::install(Some(Arc::new(Mods::open(&sources, Layers { text: false, ..Layers::ALL }))));
        for id in [POST_ESCAPE_SCENE_ID, STATION_SCENE_ID] {
            assert!(!Ff7Scenes::new(Some(adapter.clone())).build_private(id, &story, 1).unwrap().source.echo_voice(), "{id}: the text layer is off");
        }
        crate::mods::install(None);
    }

    /// The reactor's cutscenes for a player alone: Echo-S's edition is shown only where both editions tell the same story
    /// (`Outcome::same_story_as`, and the replacement must bind every entity of the scene by name); otherwise the original. This
    /// test says which, and why, so a log of it is the evidence either way.
    #[test]
    #[ignore = "requires the owner's FF7 data and verified Echo-S archive"]
    fn private_reactor_cutscenes_use_echo_only_where_both_editions_tell_the_same_story() {
        use fmv_core::{EditionProvenance, GameId, Platform};
        use fmv_ff7::script::scene::{run_section_to_end, run_to_end, REACTOR_DOOR_SCENE_ID, TEAM_TALK_SCENE_ID};
        use fmv_mods::{Layers, ModSources, Mods};
        let data = std::env::var_os("FMV_FF7_DATA").expect("FMV_FF7_DATA");
        let echo = std::env::var_os("FMV_ECHO_S_IRO").expect("FMV_ECHO_S_IRO");
        let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
        let adapter = Arc::new(Ff7Adapter::from_flevel_path(std::path::Path::new(&data).join("field/flevel.lgp"), provenance).unwrap());
        let sources = ModSources { echo_s: Some(echo.into()), ..Default::default() };
        crate::mods::install(Some(Arc::new(Mods::open(&sources, Layers::ALL))));
        for (id, moment) in [(TEAM_TALK_SCENE_ID, 6u16), (REACTOR_DOOR_SCENE_ID, 10)] {
            let story = StoryState::at(moment);
            let def = scene(id).expect("a registered scene");
            let field = adapter.load_field(&def.field).unwrap();
            let section = crate::mods::script_source().unwrap().section(&def.field).expect("Echo-S has the field");
            let original = run_to_end(field.field_file(), &def, &story, 1, &[]).unwrap();
            let why = match run_section_to_end(field.field_file(), &def, &story, 1, &section, &[]) {
                Err(e) => format!("the replacement cannot be played: {e}"),
                Ok(replacement) if original.same_story_as(&replacement) => format!("same story ({} original windows, {} Echo-S windows)", original.windows, replacement.windows),
                Ok(replacement) => format!(
                    "a different story: end {:?}/{:?}, moment {:?}/{:?}, party {:?}/{:?}, complete {}/{}, windows {}/{}, unknown {:?}",
                    original.end, replacement.end, original.writes.moment, replacement.writes.moment, original.party, replacement.party,
                    original.is_complete(), replacement.is_complete(), original.windows, replacement.windows, replacement.stats.unknown
                ),
            };
            let built = Ff7Scenes::new(Some(adapter.clone())).build_private(id, &story, 1).unwrap();
            eprintln!("{id}: shown from {} ({why})", if built.source.echo_voice() { "echo-s" } else { "the original" });
            assert_eq!(built.source.echo_voice(), why.starts_with("same story"), "{id}: Echo-S exactly when the editions agree");
        }
        crate::mods::install(None);
    }

    #[test]
    #[ignore = "requires the owner's FF7 data and verified Echo-S archive"]
    fn private_bar_uses_echo_with_compatible_choice_translation() {
        use fmv_core::{EditionProvenance, GameId, Platform};
        use fmv_mods::{Layers, ModSources, Mods};
        let data = std::env::var_os("FMV_FF7_DATA").expect("FMV_FF7_DATA");
        let echo = std::env::var_os("FMV_ECHO_S_IRO").expect("FMV_ECHO_S_IRO");
        let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
        let adapter = Arc::new(Ff7Adapter::from_flevel_path(std::path::Path::new(&data).join("field/flevel.lgp"), provenance).unwrap());
        let sources = ModSources { echo_s: Some(echo.into()), ..Default::default() };
        let mods = Arc::new(Mods::open(&sources, Layers::ALL));
        crate::mods::install(Some(mods.clone()));
        for selected in 0..2 {
            let mut builder = Ff7Scenes::new(Some(adapter.clone()));
            let mut built = builder.build_private(fmv_ff7::script::scene::PROOF_SCENE_ID, &StoryState::at(72), 1).unwrap();
            assert!(built.source.echo_voice());
            assert_eq!(built.def.trigger.entity, 9);
            let mut pending = None;
            let mut count = 0;
            let mut voices = 0;
            let mut speakers = Vec::new();
            let mut mapped = None;
            loop {
                match built.source.next(pending.take()).then {
                    fmv_ff7::script::scene::Then::End { how, .. } => { assert_eq!(how, End::Finished); break; }
                    fmv_ff7::script::scene::Then::Window(w) => {
                        count += 1;
                        if let fmv_ff7::script::dialogue::MessageOrigin::Canon { dialog, .. } = &w.message.origin {
                            voices += usize::from(mods.voice_path("mds7pb_1", w.id, *dialog, 0).is_some());
                            speakers.push(w.speaker);
                        }
                        if w.message.choice.is_some() {
                            assert!(mapped.is_none());
                            assert_eq!(built.source.bot_answer(&w, Some(selected + 1)), Some(selected));
                            mapped = built.source.server_answer(&w, Some(selected)).unwrap();
                            pending = Some(selected);
                        }
                    }
                }
                assert!(count <= 64);
            }
            assert_eq!(mapped, Some(selected + 1));
            assert_eq!(count, if selected == 0 { 7 } else { 12 });
            let expected = if selected == 0 { vec![9, 9, 9, 9, 9, 7, 9] } else { vec![9, 9, 9, 9, 9, 7, 9, 9, 7, 9, 7, 9] };
            assert_eq!(speakers, expected.into_iter().map(Some).collect::<Vec<_>>());
            assert!(voices >= if selected == 0 { 6 } else { 11 });
            let shared = builder.build(fmv_ff7::script::scene::PROOF_SCENE_ID, &StoryState::at(72), 1).unwrap();
            assert!(!shared.source.echo_voice(), "watchers use original window numbering");
        }
        crate::mods::install(Some(Arc::new(Mods::open(&sources, Layers { text: false, ..Layers::ALL }))));
        assert!(!Ff7Scenes::new(Some(adapter)).build_private(fmv_ff7::script::scene::PROOF_SCENE_ID, &StoryState::at(72), 1).unwrap().source.echo_voice());
        crate::mods::install(None);
    }
}
