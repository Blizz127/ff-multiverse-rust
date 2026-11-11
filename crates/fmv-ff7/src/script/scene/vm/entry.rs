//! Bounded entry scheduling. This module is dormant for every legacy VM constructor.

use super::*;
use super::super::play::control::entry::{EntryAudio, MovieRequestKind};

#[derive(Debug, Clone, Copy)]
pub(crate) struct MovieCommand {
    pub ordinal: u64,
    pub kind: MovieRequestKind,
    pub movie: u8,
    pub entity: u8,
    pub instruction_offset: u32,
    pub frame: u32,
    issuer: usize,
    next_pc: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct EntryVm {
    ordinal: u64,
    prepared: Option<u8>,
    pending: Option<MovieCommand>,
    issued: Option<MovieCommand>,
    counter: u16,
    audio: Vec<EntryAudio>,
}

pub(crate) enum EntryOutput {
    Boundary(VmBeat),
    Movie { stage: Vec<Timed<Stage>>, at: u32, command: MovieCommand },
    MovieBattleRefused { stage: Vec<Timed<Stage>>, at: u32, entity: u8, instruction_offset: u32 },
    Yield { stage: Vec<Timed<Stage>>, at: u32, reason: EntryWait },
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum EntryWait { Horizon, External, Idle }

impl Vm {
    pub(crate) fn enable_entry(&mut self, counter: u16) {
        self.entry = Some(EntryVm { counter, ..EntryVm::default() });
    }
    pub(crate) fn entry_clock(&self) -> u32 { self.clock }
    pub(crate) fn entry_movie_counter(&self) -> u16 { self.entry.as_ref().map_or(0, |e| e.counter) }
    pub(crate) fn take_entry_audio(&mut self) -> Vec<EntryAudio> {
        self.entry.as_mut().map_or_else(Vec::new, |e| std::mem::take(&mut e.audio))
    }

    pub(super) fn observe_entry_audio(&mut self, entity: usize, instruction: &Instruction) {
        // Preserve source cues that the existing stage does not play. These are observations,
        // not resolved mixer commands: consumers must refuse unsupported/banked operations.
        if matches!(instruction.opcode, 0xF0 | 0xF2..=0xF6 | 0xFC | 0xFD | 0xDA) {
            if let Some(entry) = &mut self.entry {
                entry.audio.push(EntryAudio { frame: self.clock, entity: Self::own(entity), instruction: instruction.clone() });
            }
        }
    }

    pub(super) fn exec_entry_movie(&mut self, idx: usize, entity: usize, ins: &Instruction) -> Flow {
        if ins.opcode == 0xFA {
            let Some((bank, address)) = Self::dest(ins, "var") else { return Flow::Scene(End::Broken) };
            self.set_word(bank, address, self.entry_movie_counter());
            return Flow::Next;
        }
        let Some(entry) = &mut self.entry else { return Flow::Scene(End::Broken) };
        // The field owns one movie channel. A second issuer cannot replace an outstanding
        // handshake, silently acknowledge it, or receive its completion.
        if entry.pending.is_some() { return Flow::Scene(End::Broken); }
        let (kind, movie) = if ins.opcode == 0xF8 {
            let Some(movie) = ins.literal("movie").and_then(|n| u8::try_from(n).ok()) else { return Flow::Scene(End::Broken) };
            (MovieRequestKind::Prepare, movie)
        } else {
            let Some(movie) = entry.prepared else { return Flow::Scene(End::Broken) };
            (MovieRequestKind::Play, movie)
        };
        let Some(ordinal) = entry.ordinal.checked_add(1) else { return Flow::Scene(End::Budget) };
        let command = MovieCommand { ordinal, kind, movie, entity: Self::own(entity), instruction_offset: ins.offset as u32,
            frame: self.clock, issuer: idx, next_pc: ins.end() };
        entry.ordinal = ordinal;
        entry.pending = Some(command);
        entry.issued = Some(command);
        // Native PMVIE/MOVIE retain the issuing instruction until its handshake completes.
        self.set_pc(idx, ins.offset);
        if let Some(thread) = self.threads.get_mut(idx) { thread.state = State::WaitExternal; }
        Flow::Yield
    }

    /// The wrapper checks the opaque token, operation, prepared asset and presentation
    /// evidence before calling this. A successful handshake releases exactly its issuer.
    pub(crate) fn accept_entry_movie(&mut self, ordinal: u64) -> bool {
        let Some(command) = self.entry.as_ref().and_then(|e| e.pending).filter(|p| p.ordinal == ordinal) else { return false };
        let Some(thread) = self.threads.get(command.issuer) else { return false };
        if thread.state != State::WaitExternal { return false; }
        let Some(entry) = self.entry.as_mut() else { return false };
        entry.pending = None;
        if command.kind == MovieRequestKind::Prepare { entry.prepared = Some(command.movie); }
        self.set_pc(command.issuer, command.next_pc);
        if let Some(thread) = self.threads.get_mut(command.issuer) {
            thread.state = State::Run;
            thread.time = thread.time.max(self.clock);
            thread.seen.clear();
        }
        self.epoch = self.epoch.wrapping_add(1);
        true
    }

    pub(crate) fn observe_entry_movie_counter(&mut self, counter: u16) {
        if let Some(entry) = &mut self.entry {
            entry.counter = counter;
            // Even counter zero is a new external observation and must wake a parked MVIEF.
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    fn entry_yield(&mut self, horizon: u32, reason: EntryWait) -> EntryOutput {
        self.clock = horizon;
        let at = self.clock.saturating_sub(self.origin);
        self.origin = self.clock;
        EntryOutput::Yield { stage: std::mem::take(&mut self.out), at, reason }
    }

    /// A separate bounded scheduler leaves legacy resume/quiet setup unchanged. No idle
    /// proof drains a retained entry: only actual RETs can complete it. Caller field time
    /// is independent from movie ordinals and never inferred from a decoder or audio clock.
    pub(crate) fn resume_entry(&mut self, script: &FieldScript<'_>, policy: ControlPolicy, horizon: u32, inclusive: bool) -> EntryOutput {
        if !self.scratch_valid() || self.refuse_invalid_native_state() {
            return EntryOutput::Boundary(self.finish(End::Broken).into());
        }
        self.window_checkpoint = None;
        // An added note is also due work: an exclusive boundary at this same tick must
        // leave it queued so observations can be sampled before the window dispatches.
        if !inclusive && self.clock == horizon && self.queued_note.is_some() {
            return self.entry_yield(horizon, EntryWait::Horizon);
        }
        if let Some((opener, note)) = self.queued_note.take() {
            let at = self.clock.saturating_sub(self.origin);
            let window = Window { at, opener: Self::own(opener), speaker: None, id: 0, geometry: None, message: note };
            self.origin = self.clock;
            self.record_window(&window, None);
            return EntryOutput::Boundary(Beat { stage: std::mem::take(&mut self.out), then: Then::Window(window) }.into());
        }
        if let Some(end) = self.done { return EntryOutput::Boundary(self.finish(end).into()); }
        loop {
            let Some(idx) = self.pick_through(Some((horizon, inclusive))) else {
                if self.threads.iter().all(|t| t.state == State::Done) {
                    return EntryOutput::Boundary(self.finish(End::Finished).into());
                }
                let reason = if self.entry.as_ref().is_some_and(|e| e.pending.is_some()) { EntryWait::External }
                else if self.threads.iter().any(|t| matches!(t.state, State::Run | State::WaitOffset(_))
                    || matches!(t.state, State::Parked(epoch) if epoch != self.epoch)) { EntryWait::Horizon }
                else { EntryWait::Idle };
                // A host horizon is a nonterminal pause. The configured execution ceiling
                // is different: scheduled work beyond it can never be admitted. External
                // IO alone still waits rather than manufacturing a budget failure.
                if inclusive && matches!(reason, EntryWait::Horizon) && horizon == self.limits.frames {
                    self.clock = horizon;
                    return EntryOutput::Boundary(self.finish(End::Budget).into());
                }
                return self.entry_yield(horizon, reason);
            };
            if self.steps >= self.limits.steps { return EntryOutput::Boundary(self.finish(End::Budget).into()); }
            // Movie/battle overlap has no admitted lifecycle in this bounded driver.
            // Refuse before executing BATTLE: its PC, variables, RNG, battle ordinal and
            // checkpoint stay untouched. Prior scheduled instructions remain real work.
            if self.entry.as_ref().is_some_and(|e| e.pending.is_some()) {
                if let Some(Thread { entity, code: Code::Script { pc }, .. }) = self.threads.get(idx) {
                    if script.bytes().get(..script.layout().code_end)
                        .is_some_and(|code| decode_at(code, *pc, 0).is_ok_and(|ins| ins.opcode == 0x70)) {
                        let (entity, instruction_offset) = (Self::own(*entity), *pc as u32);
                        self.clock = self.clock.max(self.thread_time(idx));
                        let at = self.clock.saturating_sub(self.origin);
                        self.origin = self.clock;
                        return EntryOutput::MovieBattleRefused {
                            stage: std::mem::take(&mut self.out), at, entity, instruction_offset,
                        };
                    }
                }
            }
            self.steps = self.steps.saturating_add(1);
            self.stats.steps = self.steps;
            self.clock = self.clock.max(self.thread_time(idx));
            let is_root = self.threads.get(idx).is_some_and(|t| matches!(t.code, Code::Root { .. }));
            let flow = if is_root { self.step_root(script, idx) } else { self.step_script(script, idx, Some(policy)) };
            if !self.scratch_valid() { return EntryOutput::Boundary(self.finish(End::Broken).into()); }
            if self.stats.dropped_requests != 0 {
                return EntryOutput::Boundary(self.finish(End::Budget).into());
            }
            if let Some(checkpoint) = self.control_cut.take() {
                let at = self.clock.saturating_sub(self.origin);
                self.origin = self.clock;
                return EntryOutput::Boundary(VmBeat::Control { stage: std::mem::take(&mut self.out), at, checkpoint });
            }
            if let Some(command) = self.entry.as_mut().and_then(|e| e.issued.take()) {
                let at = self.clock.saturating_sub(self.origin);
                self.origin = self.clock;
                return EntryOutput::Movie { stage: std::mem::take(&mut self.out), at, command };
            }
            match flow {
                Flow::Next | Flow::Yield => {},
                Flow::Scene(end) => return EntryOutput::Boundary(self.finish(end).into()),
                Flow::Window(opened) => {
                    if self.unresolved_offset_boundary(opened.time) { return EntryOutput::Boundary(self.finish(End::Budget).into()); }
                    let at = opened.time.saturating_sub(self.origin);
                    let geometry = self.geometry.get(&opened.id).copied();
                    let window = Window { at, opener: Self::own(opened.opener), speaker: None, id: opened.id, geometry, message: opened.message };
                    self.record_window(&window, Some(opened.instruction_offset));
                    self.pending = opened.pending;
                    if std::mem::take(&mut self.gift_not_given) { self.queued_note = Some((opened.opener, gift_note())); }
                    self.origin = opened.time;
                    self.clock = self.clock.max(opened.time);
                    return EntryOutput::Boundary(Beat { stage: std::mem::take(&mut self.out), then: Then::Window(window) }.into());
                }
            }
        }
    }
}
