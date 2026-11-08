//! A joined session: the local area, the replicated state and the network handle, shared by the
//! headless bot loop and the window loop.

use crate::net::{Joined, NetCommand, NetEvent, NetHandle};
use crate::state::ClientState;
use fmv_adapter_api::AreaRuntime;
use fmv_core::{AreaId, Pos};
use std::time::{Duration, Instant};

/// What we know about the joined area (for titles, banners and logs).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    pub tint: [u8; 3],
    pub area_id: AreaId,
    /// True for synthetic FIXTURE content, which must be labelled everywhere it is surfaced.
    pub fixture: bool,
}

pub struct Session {
    pub net: NetHandle,
    /// The area decoded from this machine's own content (used for local movement validation).
    pub area: Box<dyn AreaRuntime>,
    pub state: ClientState,
    pub info: SessionInfo,
    origin: Instant,
    /// The session already has to end because of a message from the handshake backlog; the first
    /// `pump` reports it.
    pending_end: Option<String>,
}

impl Session {
    /// Build the session from a finished handshake and apply the messages that arrived with it.
    pub fn new(net: NetHandle, joined: Joined, name: String, tint: [u8; 3]) -> Self {
        let Joined {
            entity,
            area_id,
            fixture,
            area,
            entities,
            backlog,
            ..
        } = joined;
        let spawn = entities
            .iter()
            .find(|e| e.id == entity && e.pos.is_finite())
            .map(|e| e.pos)
            .or_else(|| area.spawn_points().first().copied())
            .unwrap_or(Pos::new(0.0, 0.0, 0.0));
        let origin = Instant::now();
        let mut state = ClientState::new(entity, spawn, area_id.clone());
        let mut pending_end = None;
        for msg in backlog {
            // The handshake already refuses a Reject, a foreign snapshot and a second manifest, so
            // nothing here should end the session; if something does, say so on the first pump.
            if let Some(reason) = state.apply(0.0, msg) {
                pending_end.get_or_insert(reason);
            }
        }
        let info = SessionInfo {
            name,
            tint,
            area_id,
            fixture,
        };
        Self {
            net,
            area,
            state,
            info,
            origin,
            pending_end,
        }
    }

    /// Seconds since the session started (monotonic).
    pub fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    /// Apply everything the network thread has queued. Returns `Some(reason)` when the session
    /// ended abnormally (connection lost, or the server ended it).
    pub fn pump(&mut self) -> Option<String> {
        if let Some(reason) = self.pending_end.take() {
            return Some(reason);
        }
        while let Some(ev) = self.net.try_next() {
            let now = self.now();
            match ev {
                NetEvent::Server(msg) => {
                    if let Some(reason) = self.state.apply(now, msg) {
                        return Some(reason);
                    }
                }
                NetEvent::Rtt(rtt) => self.state.set_rtt(rtt),
                NetEvent::Closed(reason) => return Some(format!("connection to the server lost: {reason}")),
                // Only produced before the session starts.
                NetEvent::Joined(_) | NetEvent::Failed(_) => {}
            }
        }
        None
    }

    /// Send the next `MoveIntent` if one is due (rate limited, only on change).
    pub fn flush_intent(&mut self) {
        let now = self.now();
        if let Some(intent) = self.state.own.next_intent(now) {
            self.net.send(NetCommand::Intent(intent));
        }
    }

    /// Say goodbye and wait (bounded) for the network thread to flush `Leave`.
    pub fn leave(&mut self, wait: Duration) {
        self.net.leave_and_join(wait);
    }
}
