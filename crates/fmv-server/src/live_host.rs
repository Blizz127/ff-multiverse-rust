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

use crate::{LiveBattleOutcome, LiveCastView, LiveEvent, LiveField, LiveFieldHost, LiveInput, LiveSeed};

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
        if let Ok(triggers) = field.field_file().triggers() {
            let p = |v: [i16; 3]| (v[0], v[1], v[2]);
            rt.set_bg_triggers(
                triggers
                    .triggers
                    .iter()
                    .map(|t| fmv_ff7::runtime::BgTrigger { a: p(t.corner_a), b: p(t.corner_b), group: t.bg_group_id, frame: t.bg_frame_id, behavior: t.behavior, sound: t.sound_id })
                    .collect(),
            );
        }
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
            keys_held: if i.ok || i.ok_held { PAD_OK } else { 0 },
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
                Event::Window { window, dialog, ask, entity, offset } => out.push(LiveEvent::Window {
                    passive: self.rt.window_passive(window),
                    window,
                    dialog: u16::from(dialog),
                    ask,
                    entity: u16::try_from(entity).unwrap_or(u16::MAX),
                    offset: u32::try_from(offset).unwrap_or(u32::MAX),
                }),
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

    fn cast(&self) -> LiveCastView {
        let members = self.rt.cast(self.loader.as_ref());
        let id = |model: usize| u32::try_from(model).unwrap_or(u32::MAX);
        LiveCastView {
            // (named even while the scripts hide it: a client then keeps the player's character off the stage too)
            lead: self.rt.state.player_model.or(self.rt.state.pc_model).map(id),
            poses: members.iter().filter_map(|m| m.pose.as_ref().map(|(animation, frame)| fmv_protocol::LivePose { id: id(m.model), animation: animation.clone(), frame: *frame })).collect(),
            npcs: members
                .into_iter()
                .map(|m| NpcSnapshot {
                    id: id(m.model),
                    name: m.name,
                    model: m.skeleton,
                    idle_animation: m.animation,
                    pos: pos_of(m.x, m.y, m.z),
                    facing: field_direction_to_facing(m.dir),
                })
                .collect(),
        }
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
        self.rt.player_scripted()
    }

    fn player_place(&self) -> Option<(Pos, f32)> {
        self.rt.player_model().map(|m| (pos_of(m.x, m.y, m.z), field_direction_to_facing(m.dir)))
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
        self.rt.command().is_none() && !self.rt.window_waiting() && !self.rt.player_scripted()
    }

    fn countdown(&self) -> Option<u32> {
        let shown = (0..4u8).any(|w| self.rt.window_passive(w) && self.rt.windows()[usize::from(w)].open.is_some());
        (shown && self.rt.countdown_seconds() > 0).then(|| self.rt.countdown_seconds())
    }

    fn clocks(&self) -> (u32, u32) {
        (self.rt.play_time_seconds(), self.rt.countdown_seconds())
    }

    fn blocks(&self, from: Pos, to: Pos) -> bool {
        let at = |p: Pos| (p.x.round() as i32, p.y.round() as i32, p.z.round() as i32);
        let locked = |p: Pos| self.rt.triangle_at(at(p).0, at(p).1, at(p).2).is_some_and(|t| self.rt.triangle_locked(t));
        // (a player already on a locked triangle can still walk off it; someone solid stops a step towards them)
        (locked(to) && !locked(from)) || self.rt.player_step_blocked(at(from), at(to))
    }

    fn bumped(&mut self, from: Pos, to: Pos) {
        let at = |p: Pos| (p.x.round() as i32, p.y.round() as i32, p.z.round() as i32);
        self.rt.player_bumped(at(from), at(to));
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
