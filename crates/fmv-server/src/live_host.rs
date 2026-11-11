//! FF7's live fields (`--field-runtime`): the world's [`LiveFieldHost`] over the owner's own field archive, running
//! [`fmv_ff7::runtime::FieldRuntime`] for every field a governor with a complete story enters.

use std::sync::Arc;

use fmv_adapter_api::AreaKey;
use fmv_core::Pos;
use fmv_ff7::field::{section, ModelLoader};
use fmv_ff7::runtime::{Banks, Command, Event, FieldRuntime, PlayerInput};
use fmv_ff7::story::StoryState;
use fmv_ff7::{facing_to_field_direction, field_direction_to_facing, Ff7Adapter};
use fmv_protocol::NpcSnapshot;

use crate::{LiveEvent, LiveField, LiveFieldHost, LiveInput};

/// The live fields of FF7 over an opened adapter.
pub struct Ff7LiveHost {
    adapter: Arc<Ff7Adapter>,
}

impl std::fmt::Debug for Ff7LiveHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ff7LiveHost")
    }
}

impl Ff7LiveHost {
    pub fn new(adapter: Arc<Ff7Adapter>) -> Self {
        Self { adapter }
    }
}

impl LiveFieldHost for Ff7LiveHost {
    fn runs(&self, _area: &AreaKey, progression: &[u8]) -> bool {
        StoryState::decode(progression).is_some_and(|s| Banks::from_story(&s).is_some())
    }

    fn start(&self, area: &AreaKey, progression: &[u8]) -> Option<Box<dyn LiveField>> {
        let story = StoryState::decode(progression)?;
        let banks = Banks::from_story(&story)?;
        let field = self.adapter.load_field(area).ok()?;
        let script = field.field_file().section(section::SCRIPT).ok()?;
        let mut rt = FieldRuntime::new(script, banks).ok()?;
        rt.set_walkmesh(Arc::new(field.walkmesh().clone()));
        rt.init();
        let loader = field.field_file().model_loader().ok();
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
            keys_held: 0,
            keys_pressed: 0,
        });
        self.rt.tick(player);
        let mut out = Vec::new();
        for event in self.rt.drain_events() {
            match event {
                Event::Window { window, dialog, ask, .. } => out.push(LiveEvent::Window { window, dialog: u16::from(dialog), ask }),
                Event::WindowDone { window } | Event::WindowClosedByScript { window } => out.push(LiveEvent::WindowClosed { window }),
                Event::Command(command) => out.push(self.command(command)),
                Event::Control { locked } => out.push(LiveEvent::Note(format!("control locked={locked}"))),
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
