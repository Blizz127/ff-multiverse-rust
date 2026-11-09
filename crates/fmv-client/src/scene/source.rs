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

/// A scene made ready to play: its data and its source.
pub struct Built {
    pub def: SceneDef,
    pub source: Box<dyn SceneSource>,
}

/// Makes the scenes this machine can play.
pub trait SceneBuilder {
    /// The scene `scene` against `story` with `seed`. `Err` says why it cannot be played here (ids and error kinds only).
    fn build(&mut self, scene: &str, story: &StoryState, seed: u32) -> Result<Built, String>;

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
}
