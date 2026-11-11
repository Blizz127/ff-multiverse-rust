//! FF7's live fields (`--field-runtime`): the world's [`LiveFieldHost`] over the owner's own field archive, running
//! [`fmv_ff7::runtime::FieldRuntime`] for every field a governor with a complete story enters.

use std::path::PathBuf;
use std::sync::Arc;

use fmv_adapter_api::AreaKey;
use fmv_core::Pos;
use fmv_ff7::field::{section, ModelLoader};
use fmv_ff7::runtime::{Banks, Command, Event, FieldRuntime, PlayerInput};
use fmv_ff7::story::StoryState;
use fmv_ff7::{facing_to_field_direction, field_direction_to_facing, Ff7Adapter};
use fmv_protocol::NpcSnapshot;

use crate::{LiveBattleOutcome, LiveEvent, LiveField, LiveFieldHost, LiveInput, LiveSeed};

/// The pad bit of the OK button (circle) in the original's key words (`KEY`, `KEYON`, `KEYOF`).
const PAD_OK: u16 = 0x0020;

/// The live fields of FF7 over an opened adapter.
pub struct Ff7LiveHost {
    adapter: Arc<Ff7Adapter>,
    /// The owner's `char.lgp`, for every field animation's length (scripted timing waits on them).
    char_lgp: Option<PathBuf>,
}

impl std::fmt::Debug for Ff7LiveHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ff7LiveHost")
    }
}

impl Ff7LiveHost {
    pub fn new(adapter: Arc<Ff7Adapter>, char_lgp: Option<PathBuf>) -> Self {
        Self { adapter, char_lgp }
    }
}

impl LiveFieldHost for Ff7LiveHost {
    fn runs(&self, _area: &AreaKey, progression: &[u8]) -> bool {
        StoryState::decode(progression).is_some_and(|s| Banks::from_story(&s).is_some())
    }

    fn start(&self, area: &AreaKey, progression: &[u8], seed: &LiveSeed) -> Option<Box<dyn LiveField>> {
        let story = StoryState::decode(progression)?;
        let banks = Banks::from_story(&story)?;
        let field = self.adapter.load_field(area).ok()?;
        let script = field.field_file().section(section::SCRIPT).ok()?;
        let mut rt = FieldRuntime::new(script, banks).ok()?;
        rt.set_walkmesh(Arc::new(field.walkmesh().clone()));
        let loader = field.field_file().model_loader().ok();
        if let (Some(path), Some(loader)) = (&self.char_lgp, &loader) {
            if let Ok(mut lgp) = fmv_ff7::lgp::LgpArchive::open_path(path) {
                rt.set_animation_frame_counts(&fmv_ff7::runtime::AnimState::frame_counts_from(&mut lgp, loader));
            }
        }
        // What the governor carries, as the original's save would hold it.
        rt.gil = seed.gil;
        let mut save = rt.save_data().clone();
        for (item, count) in &seed.items {
            save.items.insert(*item, u8::try_from(*count).unwrap_or(99).min(99));
        }
        rt.set_save_data(save);
        if let Some(t) = seed.play_time {
            rt.set_play_time_seconds(t);
        }
        if let Some(t) = seed.countdown {
            rt.set_countdown_seconds(t);
        }
        if let Some(previous) = seed.previous_area.as_deref().and_then(|k| self.adapter.field_id_for_key(k)) {
            rt.set_previous_field(previous);
        }
        rt.init();
        Some(Box::new(Ff7Live { rt, loader, adapter: Arc::clone(&self.adapter) }))
    }
}

struct Ff7Live {
    rt: FieldRuntime,
    loader: Option<ModelLoader>,
    adapter: Arc<Ff7Adapter>,
}

fn pos_of(x: i32, y: i32, z: i32) -> Pos {
    Pos::new(x as f32, y as f32, z as f32)
}

impl LiveField for Ff7Live {
    fn advance(&mut self, input: Option<LiveInput>) -> Vec<LiveEvent> {
        let player = input.map(|i| PlayerInput {
            x: i.pos.x.round() as i32,
            y: i.pos.y.round() as i32,
            z: i.pos.z.round() as i32,
            triangle: 0,
            dir: facing_to_field_direction(i.facing),
            ok: i.ok,
            keys_held: if i.ok { PAD_OK } else { 0 },
            keys_pressed: if i.ok { PAD_OK } else { 0 },
        });
        self.rt.tick(player);
        // Tutorials are not shown yet: the field goes on as if the player read it.
        if self.rt.pending_tutorial().is_some() {
            self.rt.complete_tutorial();
        }
        let mut out = Vec::new();
        for event in self.rt.drain_events() {
            match event {
                Event::Window { window, dialog, ask, .. } => out.push(LiveEvent::Window { window, dialog: u16::from(dialog), ask }),
                Event::WindowDone { window } | Event::WindowClosedByScript { window } => out.push(LiveEvent::WindowClosed { window }),
                Event::Command(command) => out.push(self.command(command)),
                Event::Control { locked } => out.push(LiveEvent::Note(format!("control locked={locked}"))),
                Event::Gil { delta } => out.push(LiveEvent::Gil { delta }),
                Event::Item { item, delta } => out.push(LiveEvent::Item { item, delta }),
                _ => {}
            }
        }
        out
    }

    fn cast(&self) -> Vec<NpcSnapshot> {
        self.rt
            .cast(self.loader.as_ref())
            .into_iter()
            .map(|m| NpcSnapshot {
                id: u32::try_from(m.model).unwrap_or(u32::MAX),
                name: m.name,
                model: m.skeleton,
                idle_animation: m.animation,
                pos: pos_of(m.x, m.y, m.z),
                facing: field_direction_to_facing(m.dir),
            })
            .collect()
    }

    fn stage(&self) -> (Vec<u8>, Option<(u8, [u8; 3])>) {
        use fmv_ff7::script::scene::FadeBlend;
        let masks = self.rt.stage_background().masks().to_vec();
        let fade = self.rt.stage_fade_color().map(|c| {
            let blend = match c.blend {
                FadeBlend::Add => 0,
                FadeBlend::Subtract => 1,
                FadeBlend::Black => 2,
            };
            (blend, c.rgb)
        });
        (masks, fade)
    }

    fn control_locked(&self) -> bool {
        self.rt.state.character_lock
    }

    fn player_place(&self) -> Option<(Pos, f32)> {
        self.rt.pc_model().map(|m| (pos_of(m.x, m.y, m.z), field_direction_to_facing(m.dir)))
    }

    fn answer(&mut self, window: u8, choice: Option<u8>) -> bool {
        match choice {
            Some(c) => self.rt.answer(window, c),
            None => {
                self.rt.close_window(window);
                true
            }
        }
    }

    fn complete_command(&mut self) {
        self.rt.complete_command();
    }

    fn battle_over(&mut self, outcome: LiveBattleOutcome) {
        // BTRLD reads it: 0 won, 1 escaped, 2 lost (the port's encoding; the original's values are not verified).
        self.rt.set_battle_result(match outcome {
            LiveBattleOutcome::Victory => 0,
            LiveBattleOutcome::Escaped => 1,
            LiveBattleOutcome::Defeat => 2,
        });
        self.rt.complete_command();
    }

    fn idle(&self) -> bool {
        self.rt.command().is_none() && !self.rt.window_open() && !self.rt.state.character_lock
    }

    fn clocks(&self) -> (u32, u32) {
        (self.rt.play_time_seconds(), self.rt.countdown_seconds())
    }

    fn map_jumps_disabled(&self) -> bool {
        self.rt.state.map_jumps_disabled
    }

    fn battles_disabled(&self) -> bool {
        self.rt.state.battles_disabled
    }

    fn progression(&self, saved: &[u8]) -> Option<Vec<u8>> {
        let story = StoryState::decode(saved)?;
        self.rt.story_after(&story).ok().flatten().map(|s| s.encode())
    }
}

impl Ff7Live {
    fn jump(&self, field: u16, x: i16, y: i16, triangle: i16, dir: u8) -> LiveEvent {
        LiveEvent::MapJump {
            target: self.adapter.field_key_for_id(field),
            field_id: field,
            arrival: Pos::new(f32::from(x), f32::from(y), 0.0),
            facing: field_direction_to_facing(dir),
            hint: u32::try_from(triangle).ok(),
        }
    }

    fn command(&mut self, command: Command) -> LiveEvent {
        match command {
            Command::MapJump { field, x, y, triangle, dir } => self.jump(field, x, y, triangle, dir),
            // Minigames are not played yet: the field goes on to where the minigame would have left the player.
            Command::Minigame { field, x, y, triangle, dir, .. } => self.jump(field, x, y, triangle, dir),
            Command::Battle { formation } => LiveEvent::Battle { formation, party: self.rt.party().into_iter().filter(|c| *c < 9).collect() },
            Command::Movie { movie } => LiveEvent::Movie { movie },
            Command::Menu { kind, param } => LiveEvent::Menu { kind, param },
            Command::GameOver => LiveEvent::GameOver,
            Command::DiscChange { disc } => {
                self.rt.complete_command();
                LiveEvent::Note(format!("disc change to {disc}"))
            }
        }
    }
}
