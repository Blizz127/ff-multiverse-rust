//! Timed SOUND commands from a local scene. Channel 1 belongs to the run, with one
//! cancellable voice at a time; delayed decoding cannot resurrect a stopped sound.
use super::Live;
use crate::scene::{SceneSound, SoundAction};
use fmv_audio::SfxHandle;
use fmv_protocol::SceneRunId;
use std::time::Duration;

#[derive(Default)]
pub(super) struct SceneAudio {
    run: Option<SceneRunId>,
    handle: Option<SfxHandle>,
    pending: Option<(u16, u8, f64)>,
    touched: bool,
}

impl Live {
    fn stop_scene_sound(&mut self) {
        self.scene_audio.pending = None;
        if let Some(handle) = self.scene_audio.handle.take() {
            self.engine.stop_sfx_loop(handle, Duration::ZERO);
        }
    }

    fn end_scene_audio(&mut self) {
        self.stop_scene_sound();
        if self.scene_audio.touched {
            self.field.scene_channel(&self.engine, &self.log, false);
        }
        self.scene_audio = SceneAudio::default();
    }

    pub(super) fn scene_sounds(&mut self, commands: Vec<SceneSound>, now: f64) {
        if self.field.mode() == super::FieldSounds::Off {
            self.end_scene_audio();
            return;
        }
        let mut wanted = Vec::new();
        for cue in commands {
            if cue.action == SoundAction::Begin {
                self.end_scene_audio();
                self.scene_audio.run = Some(cue.run);
            }
            if self.scene_audio.run != Some(cue.run) { continue; }
            match cue.action {
                SoundAction::Begin => {},
                SoundAction::Prepare(id) => {
                    if id != 0 && self.sounds_asked.insert(id) { wanted.push(id); }
                }
                SoundAction::Play { id, pan } => {
                    self.stop_scene_sound();
                    self.field.scene_channel(&self.engine, &self.log, true);
                    self.scene_audio.touched = true;
                    if id == 0 {
                        self.say(&format!("scene sound stop ch1 run={}", cue.run.0));
                    } else {
                        if self.sounds_asked.insert(id) { wanted.push(id); }
                        self.scene_audio.pending = Some((id, pan, now));
                    }
                }
                SoundAction::End => self.end_scene_audio(),
            }
        }
        if !wanted.is_empty() { self.worker.load_sounds(wanted); }
        self.pump_worker();
        let Some((id, pan, at)) = self.scene_audio.pending else { return };
        if now - at > super::STALE_SOUND_S {
            self.scene_audio.pending = None;
            self.say(&format!("scene sound {id} missed its decode deadline; skipped"));
            return;
        }
        let Some(clip) = self.battle_clips.get(id) else { return };
        let pan = (f32::from(pan.min(127)) - 63.5) / 63.5;
        self.scene_audio.handle = Some(self.engine.play_sfx_controlled(clip, 0.8, pan));
        self.scene_audio.pending = None;
        self.say(&format!("scene sound ch1 {id} run={}", self.scene_audio.run.unwrap().0));
    }
}
