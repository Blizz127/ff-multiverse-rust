//! Timed SOUND and reviewed AKAO commands from a local scene. Channels 1 and 2 have
//! independent cancellable voices; delayed decoding cannot resurrect a stopped sound.
//! Channels 3 and 4 remain with the field's ambient plan.
use super::Live;
use crate::scene::{SceneSound, SoundAction};
use fmv_audio::SfxHandle;
use fmv_protocol::SceneRunId;
use std::time::Duration;

#[derive(Default)]
pub(super) struct SceneAudio {
    run: Option<SceneRunId>,
    channels: [Channel; 2],
}

#[derive(Default)]
struct Channel {
    handle: Option<SfxHandle>,
    pending: Option<(u16, u8, f64)>,
    touched: bool,
}

impl Live {
    fn stop_scene_sound(&mut self, index: usize) {
        let Some(channel) = self.scene_audio.channels.get_mut(index) else { return };
        channel.pending = None;
        if let Some(handle) = channel.handle.take() {
            self.engine.stop_sfx_loop(handle, Duration::ZERO);
        }
    }

    fn end_scene_audio(&mut self) {
        for index in 0..self.scene_audio.channels.len() {
            self.stop_scene_sound(index);
            if self.scene_audio.channels[index].touched {
                self.field.scene_channel(&self.engine, &self.log, index as u8 + 1, false);
            }
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
                SoundAction::Play { channel, id, pan } => {
                    let Some(index) = usize::from(channel).checked_sub(1).filter(|&index| index < self.scene_audio.channels.len()) else { continue };
                    self.stop_scene_sound(index);
                    self.field.scene_channel(&self.engine, &self.log, channel, true);
                    self.scene_audio.channels[index].touched = true;
                    if id == 0 {
                        self.say(&format!("scene sound stop ch{channel} run={}", cue.run.0));
                    } else {
                        if self.sounds_asked.insert(id) { wanted.push(id); }
                        self.scene_audio.channels[index].pending = Some((id, pan, now));
                    }
                }
                SoundAction::End => self.end_scene_audio(),
            }
        }
        if !wanted.is_empty() { self.worker.load_sounds(wanted); }
        self.pump_worker();
        for index in 0..self.scene_audio.channels.len() {
            let Some((id, pan, at)) = self.scene_audio.channels[index].pending else { continue };
            if now - at > super::STALE_SOUND_S {
                self.scene_audio.channels[index].pending = None;
                self.say(&format!("scene sound {id} missed its decode deadline; skipped"));
                continue;
            }
            let Some(clip) = self.battle_clips.get(id) else { continue };
            let pan = (f32::from(pan.min(127)) - 63.5) / 63.5;
            self.scene_audio.channels[index].handle = Some(self.engine.play_sfx_controlled(clip, 0.8, pan));
            self.scene_audio.channels[index].pending = None;
            self.say(&format!("scene sound ch{} {id} run={}", index + 1, self.scene_audio.run.unwrap().0));
        }
    }
}
